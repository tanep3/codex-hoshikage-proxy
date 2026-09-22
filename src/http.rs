use crate::{
    approval_manager::{ApprovalCapability, ApprovalDecisionRequest, ApprovalManager},
    catalog::ModelCatalogManager,
    config::CwdPolicy,
    control::{Execution, terminal},
    journal::{EventJournal, JournalEntry, now_ms},
    model::ModelError,
    permit::ProviderPermitPool,
    runtime::{CodexRuntime, RuntimeError},
    store::{ResponseMapping, ResponseStore},
    turn::{ResponseContentPart, ResponseOutputItem, ResponseRecord},
};
use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{broadcast, mpsc},
    time,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<CodexRuntime>,
    pub catalog: Arc<ModelCatalogManager>,
    pub cwd_policy: CwdPolicy,
    pub default_cwd: std::path::PathBuf,
    pub generated_images_root: Option<std::path::PathBuf>,
    pub api_key: Option<String>,
    pub native: Option<Arc<crate::native::NativeBridge>>,
    pub turn_idle_timeout: Duration,
    pub turn_stall_detection: Duration,
    pub turn_stall_confirmation_count: u32,
    pub turn_heartbeat: Duration,
    pub sandbox_mode: String,
    pub journal: Arc<EventJournal>,
    pub(crate) responses: Arc<ResponseStore>,
    pub(crate) permits: Arc<ProviderPermitPool>,
    pub approvals: Arc<ApprovalManager>,
    next_chat_id: Arc<AtomicU64>,
    thread_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    control_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

struct StartedTurn {
    model: crate::model::ResolvedModel,
    thread_id: String,
    turn_id: Option<String>,
    notifications: broadcast::Receiver<Value>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    _thread_guard: tokio::sync::OwnedMutexGuard<()>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Arc<CodexRuntime>,
        catalog: ModelCatalogManager,
        cwd_policy: CwdPolicy,
        default_cwd: std::path::PathBuf,
        api_key: Option<String>,
        turn_idle_timeout: Duration,
        turn_stall_detection: Duration,
        turn_stall_confirmation_count: u32,
        turn_heartbeat: Duration,
        sandbox_mode: String,
        approval_timeout: Duration,
        auto_approve_workspace: bool,
        journal: Arc<EventJournal>,
        responses: Arc<ResponseStore>,
    ) -> Self {
        let provider_limits = catalog.provider_limits();
        let catalog = Arc::new(catalog);
        catalog.start_status_observer();
        let approvals = ApprovalManager::new(
            Arc::clone(&runtime),
            approval_timeout,
            auto_approve_workspace,
        );
        approvals.start();
        let mut events = runtime.subscribe();
        let store = responses.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if event["kind"] == "transport_closed" {
                            break;
                        }
                        if event["method"] == "turn/completed" {
                            let params = &event["params"];
                            if let (Some(turn), Some(status)) = (
                                params
                                    .get("turnId")
                                    .or_else(|| params.pointer("/turn/id"))
                                    .and_then(Value::as_str),
                                params.pointer("/turn/status").and_then(Value::as_str),
                            ) && let Err(error) = store.control.observe(turn, status)
                            {
                                tracing::error!(%error, "terminal observation persistence failed");
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Self {
            generated_images_root: None,
            runtime,
            catalog,
            cwd_policy,
            default_cwd,
            api_key,
            native: None,
            turn_idle_timeout,
            turn_stall_detection,
            turn_stall_confirmation_count,
            turn_heartbeat,
            sandbox_mode,
            journal,
            responses,
            permits: Arc::new(ProviderPermitPool::new(provider_limits)),
            approvals,
            next_chat_id: Arc::new(AtomicU64::new(1)),
            thread_locks: Default::default(),
            control_locks: Default::default(),
        }
    }

    async fn touch_turn(&self, turn_id: &str, status: Option<&str>) {
        if let Some(status) = status
            && let Err(error) = self.responses.control.observe(turn_id, status)
        {
            tracing::error!(%error, "failed to persist turn status");
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    pub model: Option<String>,
    pub input: Value,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub reasoning: Option<ReasoningRequest>,
    #[serde(default)]
    pub text: Option<TextRequest>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub response_format: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Deserialize)]
pub struct TextRequest {
    pub format: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ReasoningRequest {
    pub effort: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
struct ApiErrorDetail {
    message: String,
    r#type: &'static str,
    code: &'static str,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(ApiErrorBody {
                error: ApiErrorDetail {
                    message: self.message,
                    r#type: "invalid_request_error",
                    code: self.code,
                },
            }),
        )
            .into_response()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/codex", get(codex_native))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/models", get(list_models))
        .route(
            "/v1/codex/responses/{response_id}/images",
            get(crate::generated_images::list),
        )
        .route(
            "/v1/codex/responses/{response_id}/images/{filename}",
            get(crate::generated_images::content),
        )
        .route("/v1/responses", post(create_response))
        .route("/v1/codex/capabilities", get(capabilities))
        .route("/v1/codex/requests/{request_id}", get(get_request))
        .route("/v1/codex/responses/{response_id}", get(get_execution))
        .route(
            "/v1/codex/turns/{turn_id}/interrupt",
            post(control_interrupt),
        )
        .route("/v1/codex/turns/{turn_id}/steer", post(control_steer))
        .route("/v1/codex/turns/{turn_id}/approvals", get(turn_approvals))
        .route("/v1/chat/completions", post(create_chat_completion))
        .route("/v1/codex/turns/{turn_id}/status", get(get_turn_status))
        .route(
            "/v1/codex/approvals/{approval_id}",
            get(get_approval).post(decide_approval),
        )
        .route(
            "/v1/codex/turns/{turn_id}/events/stream",
            get(turn_events_stream),
        )
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(
    State(state): State<AppState>,
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Response {
    let Some(expected) = state.api_key.as_deref() else {
        return next.run(request).await;
    };
    let authorized = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| {
            let supplied = Sha256::digest(value.as_bytes());
            let configured = Sha256::digest(expected.as_bytes());
            bool::from(supplied.ct_eq(&configured))
        });
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "API key is missing or invalid",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response()
    }
}

async fn codex_native(
    State(state): State<AppState>,
    websocket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let native = state.native.ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "codex_native_unavailable",
            "Codex Native API is unavailable",
        )
    })?;
    let permit = native.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "codex_native_connection_limit",
            "Codex Native API connection limit reached",
        )
    })?;
    let max_message_bytes = native.max_message_bytes();
    Ok(websocket
        .max_message_size(max_message_bytes)
        .max_frame_size(max_message_bytes)
        .on_upgrade(move |socket| native.serve(socket, permit))
        .into_response())
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": state.catalog.list_public_models().await,
    }))
}

async fn get_turn_status(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(turn_snapshot(&state, &turn_id).await?))
}

async fn get_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state
        .approvals
        .get(&approval_id)
        .await
        .map(|view| (StatusCode::OK, Json(view)))
        .map_err(approval_error)
}

async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let current = state
        .approvals
        .get(&approval_id)
        .await
        .map_err(approval_error)?;
    if request
        .expected_turn_id
        .as_deref()
        .is_some_and(|id| current.details.get("turnId").and_then(Value::as_str) != Some(id))
        || request
            .expected_thread_id
            .as_deref()
            .is_some_and(|id| current.details.get("threadId").and_then(Value::as_str) != Some(id))
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "approval_target_mismatch",
            "approval target differs",
        ));
    }
    state
        .approvals
        .decide(&approval_id, &request.decision)
        .await
        .map(|view| (StatusCode::OK, Json(view)))
        .map_err(approval_error)
}

async fn turn_events_stream(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
) -> Result<Response, ApiError> {
    execution_for_turn(&state, &turn_id)?;
    let mut notifications = state.runtime.subscribe();
    let snapshot = turn_snapshot(&state, &turn_id).await?;
    let (sender, receiver) = mpsc::channel::<Event>(32);
    let approvals = Arc::clone(&state.approvals);
    let connection_id = uuid::Uuid::new_v4().to_string();
    tokio::spawn(async move {
        let mut sequence = 0_u64;
        let mut make_event = |name: &str, value: &Value| {
            sequence += 1;
            Event::default()
                .event(name)
                .id(format!("{connection_id}:{sequence}"))
                .json_data(value)
                .unwrap_or_default()
        };
        if sender
            .send(make_event(
                "codex.events.reset",
                &json!({"replay":false,"resync_required":true}),
            ))
            .await
            .is_err()
        {
            return;
        }
        if sender
            .send(make_event("codex.turn.snapshot", &snapshot))
            .await
            .is_err()
        {
            return;
        }
        for event in approvals.pending_events_for_turn(&turn_id).await {
            if sender
                .send(make_event("approval_requested", &event))
                .await
                .is_err()
            {
                return;
            }
        }
        loop {
            let event = tokio::select! {
                _ = sender.closed() => return,
                event = notifications.recv() => event,
            };
            let event = match event {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    if sender
                        .send(make_event(
                            "codex.events.gap",
                            &json!({"skipped":skipped,"resync_required":true}),
                        ))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if event.get("kind").and_then(Value::as_str) == Some("transport_closed") {
                let _ = sender
                    .send(make_event(
                        "codex.events.gap",
                        &json!({"reason":"transport_closed","resync_required":true}),
                    ))
                    .await;
                return;
            }
            if !matches_turn_event(&event, &turn_id) {
                continue;
            }
            let name = event
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| event.get("method").and_then(Value::as_str))
                .unwrap_or("codex.event");
            if sender.send(make_event(name, &event)).await.is_err() {
                return;
            }
        }
    });
    let stream = ReceiverStream::new(receiver).map(Ok::<Event, std::convert::Infallible>);
    Ok(Sse::new(stream).into_response())
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.runtime.snapshot().await == crate::domain::RuntimeState::Ready
        && state.responses.control.records().is_ok()
    {
        (StatusCode::OK, Json(HealthBody { status: "ready" }))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthBody {
                status: "not_ready",
            }),
        )
    }
}

async fn create_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    reject_client_tools(&body)?;
    let request: ResponsesRequest = serde_json::from_value(body.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        )
    })?;
    let key = headers
        .get("idempotency-key")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_id",
                "invalid Idempotency-Key",
            )
        })?;
    if let Some(key) = &key {
        validate_request_id(key)?;
    }
    let response_id = format!("resp_{}", uuid::Uuid::new_v4());
    let fingerprint = crate::control::fingerprint(&body);
    let record = Execution::new(response_id.clone(), key, Some(fingerprint.clone()));
    if let Some(existing) = state
        .responses
        .control
        .reserve(record)
        .map_err(control_store_error)?
    {
        if existing.fingerprint.as_deref() != Some(&fingerprint) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "request_conflict",
                "request ID already used with different content",
            ));
        }
        return Ok(Json(existing.public()).into_response());
    }
    let started = match begin_turn(&state, &request, &response_id).await {
        Ok(started) => started,
        Err(error) => {
            state
                .responses
                .control
                .update(&response_id, |r| {
                    r.phase = if matches!(r.phase.as_str(), "received" | "rejected") {
                        "rejected"
                    } else {
                        "unknown"
                    }
                    .into();
                })
                .map_err(control_store_error)?;
            return Err(error);
        }
    };
    if request.stream {
        return stream_response(state, response_id, started).await;
    }
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
        _thread_guard,
    } = started;
    let text = match collect_turn_text(
        &state,
        &mut notifications,
        &thread_id,
        turn_id.as_deref(),
        state.turn_idle_timeout,
    )
    .await
    {
        Ok(text) => text,
        Err(error) => {
            interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
            return Err(error);
        }
    };

    if let Some(turn) = turn_id.as_deref() {
        state
            .responses
            .control
            .observe(turn, "completed")
            .map_err(control_store_error)?;
    }
    state
        .responses
        .put(ResponseMapping {
            response_id: response_id.clone(),
            thread_id,
            model_id: model.public_model_id.clone(),
        })
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "persistence_error",
                error.to_string(),
            )
        })?;
    let response = ResponseRecord {
        id: response_id,
        object: "response",
        model: model.public_model_id,
        output: vec![ResponseOutputItem {
            id: "msg_1".into(),
            item_type: "message",
            role: "assistant",
            content: vec![ResponseContentPart {
                part_type: "output_text",
                text,
            }],
            status: "completed",
        }],
        status: "completed",
    };
    let _ = state
        .journal
        .append(&JournalEntry {
            timestamp_ms: now_ms(),
            event: "response.completed",
            response_id: &response.id,
            model: &response.model,
            status: "completed",
        })
        .await;
    let identity = state
        .responses
        .control
        .get(&response.id)
        .map_err(control_store_error)?
        .map(|r| r.public())
        .unwrap_or(Value::Null);
    let mut response = (StatusCode::OK, Json(response)).into_response();
    add_identity_headers(&mut response, &identity);
    Ok(response)
}

async fn create_chat_completion(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    reject_client_tools(&body)?;
    let request: ChatCompletionsRequest = serde_json::from_value(body).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            error.to_string(),
        )
    })?;
    let input = chat_messages_to_input(&request.messages)?;
    let internal = ResponsesRequest {
        model: request.model.clone(),
        input: Value::Array(input),
        previous_response_id: None,
        stream: request.stream,
        metadata: request.metadata,
        reasoning: request.reasoning_effort.map(|effort| ReasoningRequest {
            effort: Some(effort),
        }),
        text: request.response_format.as_ref().map(|format| {
            let mut format = format.clone();
            if format.get("type").and_then(Value::as_str) == Some("json_schema") {
                let schema = format
                    .pointer("/json_schema/schema")
                    .cloned()
                    .unwrap_or(Value::Null);
                format = json!({"type": "json_schema", "schema": schema});
            }
            TextRequest {
                format: Some(format),
            }
        }),
    };
    let started = begin_turn_with_mode(&state, &internal, true, None).await?;
    if request.stream {
        return stream_chat_completion(state, started).await;
    }
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
        _thread_guard,
    } = started;
    let text = match collect_turn_text(
        &state,
        &mut notifications,
        &thread_id,
        turn_id.as_deref(),
        state.turn_idle_timeout,
    )
    .await
    {
        Ok(text) => text,
        Err(error) => {
            interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
            return Err(error);
        }
    };
    let response_id = format!(
        "chatcmpl_{}",
        state.next_chat_id.fetch_add(1, Ordering::Relaxed)
    );
    let _ = state
        .journal
        .append(&JournalEntry {
            timestamp_ms: now_ms(),
            event: "chat.completion.completed",
            response_id: &response_id,
            model: &model.public_model_id,
            status: "completed",
        })
        .await;
    let response = json!({
        "id": response_id,
        "object": "chat.completion",
        "created": now_ms() / 1000,
        "model": model.public_model_id,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }]
    });
    Ok((StatusCode::OK, Json(response)).into_response())
}

async fn collect_turn_text(
    state: &AppState,
    notifications: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    turn_id: Option<&str>,
    idle_timeout: Duration,
) -> Result<String, ApiError> {
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(
            idle_timeout,
            recv_turn_event(notifications, thread_id, turn_id),
        )
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "runtime_timeout",
                "Codex turn timed out",
            )
        })?
        .map_err(|error| runtime_error(RuntimeError::Protocol(error.to_string())))?;
        if let Some(code) = interaction_error(&event, thread_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                code,
                "client cannot handle the requested interaction",
            ));
        }
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").cloned().unwrap_or_else(|| json!({}));
        if !matches_thread_and_turn(&params, thread_id, turn_id) {
            continue;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            "turn/completed" => {
                let status = params
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                if let Some(turn_id) = turn_id {
                    state
                        .responses
                        .control
                        .observe(turn_id, status)
                        .map_err(control_store_error)?;
                    state.approvals.invalidate_turn(thread_id, turn_id).await;
                }
                if status != "completed" {
                    let detail = params
                        .pointer("/turn/error")
                        .cloned()
                        .unwrap_or_else(|| params.clone());
                    return Err(ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "turn_failed",
                        format!("Codex turn ended with status {status}: {detail}"),
                    ));
                }
                if text.is_empty() {
                    text = collect_text(&params);
                }
                return Ok(text);
            }
            _ => {}
        }
    }
}

async fn stream_chat_completion(
    state: AppState,
    started: StartedTurn,
) -> Result<Response, ApiError> {
    let id = format!(
        "chatcmpl_{}",
        state.next_chat_id.fetch_add(1, Ordering::Relaxed)
    );
    let (sender, receiver) = mpsc::channel::<Event>(32);
    let turn_id = started.turn_id.clone();
    tokio::spawn(run_chat_stream(state, id, started, sender));
    let stream = ReceiverStream::new(receiver).map(Ok::<Event, std::convert::Infallible>);
    let mut response = Sse::new(stream).into_response();
    if let Some(turn_id) = turn_id
        && let Ok(value) = turn_id.parse()
    {
        response.headers_mut().insert("x-codex-turn-id", value);
    }
    Ok(response)
}

async fn run_chat_stream(
    state: AppState,
    id: String,
    started: StartedTurn,
    sender: mpsc::Sender<Event>,
) {
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
        _thread_guard,
    } = started;
    let created = now_ms() / 1000;
    let role_chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model.public_model_id,
        "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
    });
    if sender.send(sse_data(&role_chunk)).await.is_err() {
        interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
        return;
    }
    let _ = state
        .journal
        .append(&JournalEntry {
            timestamp_ms: now_ms(),
            event: "chat.completion.created",
            response_id: &id,
            model: &model.public_model_id,
            status: "in_progress",
        })
        .await;
    loop {
        let event = tokio::select! {
            _ = sender.closed() => {
                interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
                return;
            }
            event = time::timeout(state.turn_idle_timeout, recv_turn_event(&mut notifications, &thread_id, turn_id.as_deref())) => event,
        };
        let event = match event {
            Ok(Ok(event)) => event,
            error => {
                interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
                let message = match error {
                    Err(_) => "Codex turn timed out".to_owned(),
                    Ok(Err(error)) => format!("Codex event stream unavailable: {error}"),
                    Ok(Ok(_)) => unreachable!(),
                };
                let _ = sender.send(sse_data(&json!({"error": message}))).await;
                let _ = sender.send(sse_done()).await;
                return;
            }
        };
        if let Some(code) = interaction_error(&event, &thread_id) {
            interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
            let _ = sender
                .send(sse_data(&json!({"error": {"code": code}})))
                .await;
            let _ = sender.send(sse_done()).await;
            return;
        }
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").cloned().unwrap_or_else(|| json!({}));
        if !matches_thread_and_turn(&params, &thread_id, turn_id.as_deref()) {
            continue;
        }
        if let Some(turn_id) = turn_id.as_deref() {
            let status = (method == "turn/completed")
                .then(|| params.pointer("/turn/status").and_then(Value::as_str))
                .flatten();
            state.touch_turn(turn_id, status).await;
        }
        if method == "item/agentMessage/delta" {
            let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                continue;
            };
            let chunk = json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model.public_model_id,
                "choices": [{"index": 0, "delta": {"content": delta}, "finish_reason": null}]
            });
            if sender.send(sse_data(&chunk)).await.is_err() {
                tracing::info!(?turn_id, "client disconnected; interrupting chat turn");
                if let Some(turn_id) = turn_id.as_deref() {
                    let _ = state
                        .runtime
                        .request(
                            "turn/interrupt",
                            json!({"threadId": thread_id, "turnId": turn_id}),
                        )
                        .await;
                }
                return;
            }
            let _ = state
                .journal
                .append(&JournalEntry {
                    timestamp_ms: now_ms(),
                    event: "chat.completion.delta",
                    response_id: &id,
                    model: &model.public_model_id,
                    status: "in_progress",
                })
                .await;
        } else if method == "turn/completed" {
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            if status != "completed" {
                let _ = sender
                    .send(sse_data(
                        &json!({"error": turn_failure_message(status, &params)}),
                    ))
                    .await;
                let _ = sender.send(sse_done()).await;
                return;
            }
            let finish = json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model.public_model_id,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            });
            let _ = sender.send(sse_data(&finish)).await;
            let _ = state
                .journal
                .append(&JournalEntry {
                    timestamp_ms: now_ms(),
                    event: "chat.completion.completed",
                    response_id: &id,
                    model: &model.public_model_id,
                    status: "completed",
                })
                .await;
            let _ = sender.send(sse_done()).await;
            return;
        }
    }
}

fn chat_messages_to_input(messages: &[ChatMessage]) -> Result<Vec<Value>, ApiError> {
    if messages.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "messages must not be empty",
        ));
    }
    let mut input = Vec::new();
    for message in messages {
        input.extend(normalize_message(&message.role, &message.content)?);
    }
    Ok(input)
}

async fn begin_turn(
    state: &AppState,
    request: &ResponsesRequest,
    response_id: &str,
) -> Result<StartedTurn, ApiError> {
    begin_turn_with_mode(state, request, false, Some(response_id)).await
}

async fn begin_turn_with_mode(
    state: &AppState,
    request: &ResponsesRequest,
    ephemeral: bool,
    response_id: Option<&str>,
) -> Result<StartedTurn, ApiError> {
    // Lock a known conversation before waiting for provider capacity. Otherwise
    // a competing request could wait out the active turn and silently run later.
    let previous = if let Some(id) = request.previous_response_id.as_deref() {
        if let Some(record) = state
            .responses
            .control
            .get(id)
            .map_err(control_store_error)?
            && (record.last_observed_status != "completed" || record.phase != "finished")
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "response_not_continuable",
                "only successful responses may be continued",
            ));
        }
        Some(state.responses.get(id).await.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "thread_not_found",
                "previous response was not found",
            )
        })?)
    } else {
        None
    };
    let mut control_guard = None;
    let thread_guard = if let Some(context) = &previous {
        let guard = lock_thread(state, &context.thread_id).await?;
        let lock = state
            .control_locks
            .lock()
            .await
            .entry(context.thread_id.clone())
            .or_default()
            .clone();
        control_guard = Some(lock.lock_owned().await);
        check_thread_idle(state, &context.thread_id).await?;
        Some(guard)
    } else {
        None
    };
    let inherited_model = if let Some(context) = &previous {
        state
            .responses
            .control
            .records()
            .map_err(control_store_error)?
            .into_iter()
            .filter(|r| {
                r.thread_id.as_deref() == Some(&context.thread_id)
                    && r.turn_id.is_some()
                    && r.model_id.is_some()
            })
            .max_by_key(|r| r.sequence)
            .and_then(|r| r.model_id)
            .or_else(|| Some(context.model_id.clone()))
    } else {
        None
    };
    let reasoning = request
        .reasoning
        .as_ref()
        .and_then(|value| value.effort.as_deref());
    let model = state
        .catalog
        .resolve(
            request.model.as_deref().or(inherited_model.as_deref()),
            reasoning,
        )
        .await
        .map_err(model_error)?;
    if let Some(context) = &previous
        && context.model_id.split('/').next() != Some(model.public_provider_id.as_str())
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "cross_provider_model_change_unsupported",
            "conversation model changes are supported within the same public provider only",
        ));
    }
    let explicit_cwd = request
        .metadata
        .get("codex.cwd")
        .map(|value| state.cwd_policy.validate(value))
        .transpose()
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_cwd",
                "invalid working directory",
            )
        })?;
    let cwd = if let Some(context) = &previous {
        let saved = state
            .responses
            .control
            .records()
            .map_err(control_store_error)?
            .into_iter()
            .filter(|r| r.thread_id.as_deref() == Some(&context.thread_id) && r.turn_id.is_some())
            .filter_map(|r| r.cwd.map(|cwd| (r.sequence, cwd)))
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, cwd)| cwd);
        let saved = saved.ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "workspace_unknown",
                "historical workspace is not recorded",
            )
        })?;
        let saved = state.cwd_policy.validate(&saved).map_err(|_| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "workspace_access_revoked",
                "workspace access revoked",
            )
        })?;
        if explicit_cwd.as_ref().is_some_and(|cwd| cwd != &saved) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "workspace_change_unsupported",
                "continuation cannot change workspace",
            ));
        }
        saved
    } else {
        explicit_cwd.unwrap_or_else(|| state.default_cwd.clone())
    };
    let input = normalize_input(&request.input)?;
    let output_schema =
        parse_output_schema(request.text.as_ref().and_then(|text| text.format.as_ref()))?;
    let suppress_auto_approval = match request
        .metadata
        .get("codex.auto_approve_workspace")
        .map(String::as_str)
    {
        None | Some("true") => false,
        Some("false") => true,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_approval_policy",
                "codex.auto_approve_workspace must be true or false",
            ));
        }
    };
    let generated_id = format!("chat_{}", uuid::Uuid::new_v4());
    let response_id = response_id.unwrap_or(&generated_id);
    if ephemeral {
        state
            .responses
            .control
            .reserve(Execution::new(response_id.into(), None, None))
            .map_err(control_store_error)?;
    }
    drop(control_guard.take());
    let permit = state
        .permits
        .acquire(&model.public_provider_id)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unavailable",
                e.to_string(),
            )
        })?;
    if let Some(context) = &previous {
        let lock = state
            .control_locks
            .lock()
            .await
            .entry(context.thread_id.clone())
            .or_default()
            .clone();
        control_guard = Some(lock.lock_owned().await);
    }
    let resuming = previous.is_some();
    // Persist before either upstream call; a cancelled future must not lose this boundary.
    state
        .responses
        .control
        .update(response_id, |r| {
            r.phase = "dispatching".into();
            r.thread_id = previous.as_ref().map(|c| c.thread_id.clone());
            r.model_id = Some(model.public_model_id.clone());
            r.cwd = Some(cwd.to_string_lossy().into_owned());
        })
        .map_err(control_store_error)?;
    let thread_id = if let Some(context) = previous {
        let result = state.runtime.request("thread/resume",json!({
            "threadId":context.thread_id,"model":model.upstream_model_id,"modelProvider":model.codex_provider_id,
            "cwd":cwd,"approvalPolicy":"on-request","sandbox":state.sandbox_mode
        })).await;
        result.map_err(|error| start_error(state, response_id, error, resuming))?;
        context.thread_id
    } else {
        let result = state.runtime.request("thread/start",json!({
            "model":model.upstream_model_id,"modelProvider":model.codex_provider_id,"cwd":cwd,"ephemeral":ephemeral,
            "approvalPolicy":"on-request","sandbox":state.sandbox_mode
        })).await.map_err(|error|start_error(state,response_id,error,false))?;
        string_at(&result, &["thread", "id"])
            .or_else(|| result.get("id").and_then(Value::as_str))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "runtime_error",
                    "thread/start returned no thread id",
                )
            })?
            .to_owned()
    };
    let thread_guard = match thread_guard {
        Some(guard) => guard,
        None => lock_thread(state, &thread_id).await?,
    };
    if control_guard.is_none() {
        let lock = state
            .control_locks
            .lock()
            .await
            .entry(thread_id.clone())
            .or_default()
            .clone();
        control_guard = Some(lock.lock_owned().await);
    }
    let _control_guard = control_guard;
    let suppress_auto_approval = suppress_auto_approval
        || state
            .responses
            .control
            .records()
            .map_err(control_store_error)?
            .iter()
            .any(|r| r.thread_id.as_deref() == Some(&thread_id) && r.suppress_auto_approval);
    state
        .responses
        .control
        .update(response_id, |r| {
            r.thread_id = Some(thread_id.clone());
            r.model_id = Some(model.public_model_id.clone());
            r.suppress_auto_approval = suppress_auto_approval;
        })
        .map_err(control_store_error)?;
    let notifications = state.runtime.subscribe();
    let capability = if request
        .metadata
        .get("codex.approval_capability")
        .map(String::as_str)
        == Some("interactive")
    {
        ApprovalCapability::Interactive
    } else {
        ApprovalCapability::None
    };
    state
        .approvals
        .register_turn(&thread_id, capability, &cwd, suppress_auto_approval)
        .await;
    let turn_result = state
        .runtime
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": input,
                "model": model.upstream_model_id,
                "cwd": cwd,
                "effort": model.reasoning_effort.map(reasoning_name),
                "outputSchema": output_schema,
                "approvalPolicy": "on-request"
            }),
        )
        .await
        .map_err(|error| start_error(state, response_id, error, resuming))?;
    let turn_id = string_at(&turn_result, &["turn", "id"])
        .or_else(|| turn_result.get("id").and_then(Value::as_str))
        .map(str::to_owned);
    if let Err(error) = state.responses.control.update(response_id, |r| {
        r.turn_id = turn_id.clone();
        r.phase = if turn_id.is_some() {
            "started"
        } else {
            "unknown"
        }
        .into();
        r.last_observed_status = if turn_id.is_some() {
            "inProgress"
        } else {
            "unknown"
        }
        .into();
        r.last_observed_at_ms = now_ms();
    }) {
        interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
        return Err(control_store_error(error));
    }
    if let Some(turn_id) = turn_id.as_deref() {
        state.approvals.bind_turn(&thread_id, turn_id).await;
    }
    if turn_id.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "turn_identity_unknown",
            "upstream omitted turn identity; do not replay",
        ));
    }
    Ok(StartedTurn {
        model,
        thread_id,
        turn_id,
        notifications,
        _permit: permit,
        _thread_guard: thread_guard,
    })
}

async fn stream_response(
    state: AppState,
    response_id: String,
    started: StartedTurn,
) -> Result<Response, ApiError> {
    let created = ResponseRecord {
        id: response_id.clone(),
        object: "response",
        model: started.model.public_model_id.clone(),
        output: Vec::new(),
        status: "in_progress",
    };
    let (sender, receiver) = mpsc::channel::<Event>(32);
    let _ = state
        .journal
        .append(&JournalEntry {
            timestamp_ms: now_ms(),
            event: "response.created",
            response_id: &response_id,
            model: &started.model.public_model_id,
            status: "in_progress",
        })
        .await;
    sender
        .send(sse_json("response.created", &created).map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "stream_error",
                error.to_string(),
            )
        })?)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "stream_error",
                "stream closed",
            )
        })?;
    let identity = json!({"response_id": response_id, "thread_id": started.thread_id, "turn_id": started.turn_id});
    tokio::spawn(run_stream(state, response_id, started, sender));
    let stream = ReceiverStream::new(receiver).map(Ok::<Event, std::convert::Infallible>);
    let mut response = Sse::new(stream).into_response();
    add_identity_headers(&mut response, &identity);
    Ok(response)
}

async fn run_stream(
    state: AppState,
    response_id: String,
    started: StartedTurn,
    sender: mpsc::Sender<Event>,
) {
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
        _thread_guard,
    } = started;
    let mut silent_since = Instant::now();
    let mut stalled_probe_count = 0_u32;
    loop {
        let remaining = state
            .turn_idle_timeout
            .checked_sub(silent_since.elapsed())
            .unwrap_or_default();
        let probe_after = remaining.min(state.turn_heartbeat);
        let event = tokio::select! {
            _ = sender.closed() => {
                interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
                return;
            }
            event = time::timeout(probe_after, recv_turn_event(&mut notifications, &thread_id, turn_id.as_deref())) => event,
        };
        let event = match event {
            Ok(Ok(event)) => {
                silent_since = Instant::now();
                stalled_probe_count = 0;
                event
            }
            Err(_) => {
                if silent_since.elapsed() < state.turn_idle_timeout {
                    if silent_since.elapsed() < state.turn_stall_detection {
                        send_turn_heartbeat(
                            &sender,
                            &response_id,
                            turn_id.as_deref(),
                            "Codex turn is still running",
                        )
                        .await;
                        continue;
                    }
                    send_turn_heartbeat(
                        &sender,
                        &response_id,
                        turn_id.as_deref(),
                        "Checking Codex turn status for stalled progress",
                    )
                    .await;
                    match probe_turn(&state, &thread_id, turn_id.as_deref()).await {
                        Ok(probe) if probe.waiting_for_user => {
                            send_turn_heartbeat(
                                &sender,
                                &response_id,
                                turn_id.as_deref(),
                                "Codex turn is waiting for approval or user input",
                            )
                            .await;
                            tracing::info!(
                                response_id = %response_id,
                                thread_id = %thread_id,
                                turn_id = ?turn_id,
                                "Codex Turn is waiting for user input; watchdog remains active"
                            );
                            continue;
                        }
                        Ok(probe) if probe.status == "inProgress" => {
                            stalled_probe_count += 1;
                            tracing::warn!(
                                response_id = %response_id,
                                thread_id = %thread_id,
                                turn_id = ?turn_id,
                                probe = stalled_probe_count,
                                required = state.turn_stall_confirmation_count,
                                silent_seconds = silent_since.elapsed().as_secs(),
                                "Codex Turn has no progress event"
                            );
                            if stalled_probe_count >= state.turn_stall_confirmation_count {
                                let message = format!(
                                    "Codex Turn remained inProgress without events for {} seconds across {} checks",
                                    silent_since.elapsed().as_secs(),
                                    stalled_probe_count
                                );
                                fail_response_stream(
                                    &state,
                                    &sender,
                                    &response_id,
                                    &model.public_model_id,
                                    (&thread_id, turn_id.as_deref()),
                                    "turn_stalled",
                                    message,
                                )
                                .await;
                                break;
                            }
                        }
                        Ok(probe) => {
                            send_turn_heartbeat(
                                &sender,
                                &response_id,
                                turn_id.as_deref(),
                                &format!("Codex turn status: {}", probe.status),
                            )
                            .await;
                            tracing::warn!(
                                response_id = %response_id,
                                thread_id = %thread_id,
                                turn_id = ?turn_id,
                                status = %probe.status,
                                "Codex Turn status changed while response had no events"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                response_id = %response_id,
                                thread_id = %thread_id,
                                turn_id = ?turn_id,
                                error = %error,
                                "Codex Turn watchdog probe failed; continuing until hard timeout"
                            );
                        }
                    }
                    continue;
                }
                let code = "runtime_idle_timeout";
                let message = format!(
                    "Codex produced no event for {} seconds",
                    state.turn_idle_timeout.as_secs()
                );
                tracing::warn!(
                    response_id = %response_id,
                    thread_id = %thread_id,
                    turn_id = ?turn_id,
                    error_code = code,
                    error_message = %message,
                    idle_timeout_seconds = state.turn_idle_timeout.as_secs(),
                    "Codex turn stream failed"
                );
                if let Some(turn_id) = turn_id.as_deref() {
                    let _ = state
                        .runtime
                        .request(
                            "turn/interrupt",
                            json!({"threadId": thread_id, "turnId": turn_id}),
                        )
                        .await;
                }
                let _ = sender
                    .send(
                        sse_json(
                            "response.failed",
                            &json!({"id": response_id, "status": "failed", "error": {"code": code, "message": message}}),
                        )
                        .unwrap_or_else(|_| Event::default()),
                    )
                    .await;
                let _ = state
                    .journal
                    .append(&JournalEntry {
                        timestamp_ms: now_ms(),
                        event: "response.failed",
                        response_id: &response_id,
                        model: &model.public_model_id,
                        status: "failed",
                    })
                    .await;
                break;
            }
            Ok(Err(error)) => {
                let code = "runtime_disconnected";
                let message = format!("Codex event stream closed: {error}");
                tracing::warn!(
                    response_id = %response_id,
                    thread_id = %thread_id,
                    turn_id = ?turn_id,
                    error_code = code,
                    error_message = %message,
                    "Codex turn stream failed"
                );
                let _ = sender
                    .send(
                        sse_json(
                            "response.failed",
                            &json!({"id": response_id, "status": "failed", "error": {"code": code, "message": message}}),
                        )
                        .unwrap_or_else(|_| Event::default()),
                    )
                    .await;
                let _ = state
                    .journal
                    .append(&JournalEntry {
                        timestamp_ms: now_ms(),
                        event: "response.failed",
                        response_id: &response_id,
                        model: &model.public_model_id,
                        status: "failed",
                    })
                    .await;
                break;
            }
        };
        if let Some(code) = interaction_error(&event, &thread_id) {
            interrupt_turn(&state.runtime, &thread_id, turn_id.as_deref()).await;
            let _ = sender
                .send(
                    sse_json(
                        "response.failed",
                        &json!({
                            "id": response_id,
                            "status": "failed",
                            "error": {"code": code}
                        }),
                    )
                    .unwrap_or_else(|_| Event::default()),
                )
                .await;
            break;
        }
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").cloned().unwrap_or_else(|| json!({}));
        if !matches_thread_and_turn(&params, &thread_id, turn_id.as_deref()) {
            continue;
        }
        if let Some(turn_id) = turn_id.as_deref() {
            let status = (method == "turn/completed")
                .then(|| params.pointer("/turn/status").and_then(Value::as_str))
                .flatten();
            state.touch_turn(turn_id, status).await;
        }
        if method == "item/agentMessage/delta" {
            let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                continue;
            };
            let payload =
                json!({"id": response_id, "output_index": 0, "item_id": "msg_1", "delta": delta});
            if sender
                .send(
                    sse_json("response.output_text.delta", &payload)
                        .unwrap_or_else(|_| Event::default()),
                )
                .await
                .is_err()
            {
                tracing::info!(?turn_id, "client disconnected; interrupting response turn");
                if let Some(turn_id) = turn_id.as_deref() {
                    let _ = state
                        .runtime
                        .request(
                            "turn/interrupt",
                            json!({"threadId": thread_id, "turnId": turn_id}),
                        )
                        .await;
                }
                break;
            }
            let _ = state
                .journal
                .append(&JournalEntry {
                    timestamp_ms: now_ms(),
                    event: "response.output_text.delta",
                    response_id: &response_id,
                    model: &model.public_model_id,
                    status: "in_progress",
                })
                .await;
        } else if method == "turn/completed" {
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            if status != "completed" {
                let detail = params
                    .pointer("/turn/error")
                    .cloned()
                    .unwrap_or_else(|| params.clone());
                let _ = sender
                    .send(
                        sse_json(
                            "response.failed",
                            &json!({"id": response_id, "status": "failed", "error": detail}),
                        )
                        .unwrap_or_else(|_| Event::default()),
                    )
                    .await;
                let _ = state
                    .journal
                    .append(&JournalEntry {
                        timestamp_ms: now_ms(),
                        event: "response.failed",
                        response_id: &response_id,
                        model: &model.public_model_id,
                        status: "failed",
                    })
                    .await;
                break;
            }
            if state
                .responses
                .put(ResponseMapping {
                    response_id: response_id.clone(),
                    thread_id,
                    model_id: model.public_model_id.clone(),
                })
                .await
                .is_err()
            {
                let _ = sender
                    .send(
                        sse_json(
                            "response.failed",
                            &json!({
                                "id": response_id,
                                "status": "failed",
                                "error": {"code": "persistence_error"}
                            }),
                        )
                        .unwrap_or_else(|_| Event::default()),
                    )
                    .await;
                break;
            }
            let completed = json!({"id": response_id, "object": "response", "status": "completed", "model": model.public_model_id});
            let _ = sender
                .send(
                    sse_json("response.completed", &completed).unwrap_or_else(|_| Event::default()),
                )
                .await;
            let _ = state
                .journal
                .append(&JournalEntry {
                    timestamp_ms: now_ms(),
                    event: "response.completed",
                    response_id: &response_id,
                    model: &model.public_model_id,
                    status: "completed",
                })
                .await;
            break;
        }
    }
}

async fn send_turn_heartbeat(
    sender: &mpsc::Sender<Event>,
    response_id: &str,
    turn_id: Option<&str>,
    message: &str,
) {
    let _ = sender
        .send(
            sse_json(
                "codex.turn.status",
                &json!({
                    "id": response_id,
                    "turn_id": turn_id,
                    "status": "inProgress",
                    "message": message
                }),
            )
            .unwrap_or_else(|_| Event::default()),
        )
        .await;
}

struct TurnProbe {
    status: String,
    waiting_for_user: bool,
}

async fn probe_turn(
    state: &AppState,
    thread_id: &str,
    turn_id: Option<&str>,
) -> Result<TurnProbe, RuntimeError> {
    let result = state
        .runtime
        .request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await?;
    let thread = result.get("thread").unwrap_or(&result);
    let waiting_for_user = thread
        .pointer("/status/activeFlags")
        .and_then(Value::as_array)
        .is_some_and(|flags| {
            flags.iter().any(|flag| {
                matches!(
                    flag.as_str(),
                    Some("waitingOnApproval") | Some("waitingOnUserInput")
                )
            })
        });
    let status = thread
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns.iter().rev().find(|turn| {
                turn_id.is_none_or(|id| turn.get("id").and_then(Value::as_str) == Some(id))
            })
        })
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    Ok(TurnProbe {
        status,
        waiting_for_user,
    })
}

async fn fail_response_stream(
    state: &AppState,
    sender: &mpsc::Sender<Event>,
    response_id: &str,
    model: &str,
    turn: (&str, Option<&str>),
    code: &str,
    message: String,
) {
    let (thread_id, turn_id) = turn;
    tracing::warn!(
        response_id,
        thread_id,
        turn_id,
        error_code = code,
        error_message = %message,
        "Codex response failed"
    );
    if let Some(turn_id) = turn_id {
        let _ = state
            .runtime
            .request(
                "turn/interrupt",
                json!({"threadId": thread_id, "turnId": turn_id}),
            )
            .await;
    }
    let _ = sender
        .send(
            sse_json(
                "response.failed",
                &json!({
                    "id": response_id,
                    "status": "failed",
                    "error": {"code": code, "message": message}
                }),
            )
            .unwrap_or_else(|_| Event::default()),
        )
        .await;
    let _ = state
        .journal
        .append(&JournalEntry {
            timestamp_ms: now_ms(),
            event: "response.failed",
            response_id,
            model,
            status: "failed",
        })
        .await;
}

fn sse_json<T: Serialize>(event: &str, value: &T) -> Result<Event, axum::Error> {
    Event::default().event(event).json_data(value)
}

fn sse_data<T: Serialize>(value: &T) -> Event {
    Event::default()
        .json_data(value)
        .unwrap_or_else(|_| Event::default())
}

fn sse_done() -> Event {
    Event::default().data("[DONE]")
}

fn invalid_input(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request_error", message)
}

fn unsupported_input(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "unsupported_parameter", message)
}

// Codex executes its own tools. It does not hand a client-defined function call
// back to the OpenAI caller. Silently dropping these fields would execute a
// different request and can produce a false successful response.
fn reject_client_tools(body: &Value) -> Result<(), ApiError> {
    for key in [
        "tools",
        "functions",
        "tool_choice",
        "function_call",
        "parallel_tool_calls",
    ] {
        if let Some(value) = body.get(key).filter(|v| !v.is_null()) {
            let no_op = match key {
                "tools" | "functions" => value.as_array().is_some_and(Vec::is_empty),
                "tool_choice" | "function_call" => value == "none",
                "parallel_tool_calls" => value == false,
                _ => false,
            };
            if !no_op {
                return Err(unsupported_input(&format!(
                    "client-defined tool calling is not supported: {key}"
                )));
            }
        }
    }
    for key in ["messages", "input"] {
        if let Some(items) = body[key].as_array() {
            for item in items {
                if item.get("tool_calls").is_some_and(|v| !v.is_null())
                    || item.get("function_call").is_some_and(|v| !v.is_null())
                    || item.get("tool_call_id").is_some()
                {
                    return Err(unsupported_input(
                        "client tool-call history is not supported",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn normalize_message(role: &str, content: &Value) -> Result<Vec<Value>, ApiError> {
    if !matches!(role, "system" | "developer" | "user" | "assistant") {
        return Err(unsupported_input(
            "only system, developer, user and assistant messages are supported",
        ));
    }
    if let Some(text) = content.as_str() {
        return Ok(vec![
            json!({"type":"text", "text":format!("[{role}]\n{text}")}),
        ]);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| invalid_input("message content must be a string or array"))?;
    let mut input = vec![json!({"type":"text", "text":format!("[{role}]\n")})];
    for part in parts {
        input.push(normalize_content_part(part)?);
    }
    Ok(input)
}

fn normalize_content_part(part: &Value) -> Result<Value, ApiError> {
    match part.get("type").and_then(Value::as_str) {
        Some("text" | "input_text" | "output_text") => {
            let text = part
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_input("text content must have a string text field"))?;
            if part["type"] == "text" {
                return Ok(part.clone());
            }
            Ok(json!({"type":"text", "text":text}))
        }
        Some("image" | "input_image" | "image_url") => {
            let url = match part["type"].as_str() {
                Some("input_image") => part.get("image_url"),
                Some("image_url") => part.pointer("/image_url/url"),
                _ => part.get("url"),
            }
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_input("image content requires an image URL"))?;
            let parsed =
                reqwest::Url::parse(url).map_err(|_| invalid_input("invalid image URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                && !(parsed.scheme() == "data" && url.starts_with("data:image/"))
            {
                return Err(unsupported_input(
                    "images require HTTP(S) URLs or image data URLs",
                ));
            }
            let detail = if part["type"] == "image_url" {
                part.pointer("/image_url/detail")
            } else {
                part.get("detail")
            };
            let mut image = json!({"type":"image", "url":url});
            if let Some(detail) = detail.filter(|value| !value.is_null()) {
                if !matches!(detail.as_str(), Some("auto" | "low" | "high" | "original")) {
                    return Err(invalid_input("invalid image detail"));
                }
                image["detail"] = detail.clone();
            }
            Ok(image)
        }
        _ => Err(unsupported_input(
            "only text and image content is supported",
        )),
    }
}

pub(crate) fn normalize_input(input: &Value) -> Result<Vec<Value>, ApiError> {
    if let Some(text) = input.as_str() {
        return Ok(vec![json!({"type":"text", "text":text})]);
    }
    let items = input
        .as_array()
        .ok_or_else(|| invalid_input("input must be a string or array"))?;
    let mut result = Vec::new();
    for item in items {
        if let Some(role) = item.get("role").and_then(Value::as_str) {
            if item.get("type").is_some_and(|kind| kind != "message") {
                return Err(unsupported_input("unsupported message type"));
            }
            result.extend(normalize_message(role, &item["content"])?);
        } else {
            result.push(normalize_content_part(item)?);
        }
    }
    Ok(result)
}

pub(crate) fn parse_output_schema(format: Option<&Value>) -> Result<Option<Value>, ApiError> {
    let Some(format) = format else {
        return Ok(None);
    };
    match format.get("type").and_then(Value::as_str) {
        Some("text") => Ok(None),
        Some("json_schema") => {
            let schema = format
                .get("schema")
                .filter(|schema| schema.is_object())
                .ok_or_else(|| invalid_input("json_schema format requires a schema object"))?;
            Ok(Some(schema.clone()))
        }
        _ => Err(unsupported_input(
            "supported output formats are text and json_schema",
        )),
    }
}

pub(crate) async fn request_interrupt(
    runtime: &CodexRuntime,
    thread_id: &str,
    turn_id: &str,
) -> Result<Value, RuntimeError> {
    // Codex 0.153.4 may acknowledge turn/start before registering its active
    // task. Retry only this explicit rejection, never a lost RPC response.
    for attempt in 0..20 {
        let result = runtime
            .request(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
            )
            .await;
        let not_active = matches!(&result, Err(RuntimeError::Rpc { code: -32600, message }) if message.contains("no active turn to interrupt"));
        if !not_active || attempt == 19 {
            return result;
        }
        time::sleep(Duration::from_millis(50)).await;
    }
    unreachable!("bounded interrupt retry loop returns")
}

async fn interrupt_turn(runtime: &CodexRuntime, thread_id: &str, turn_id: Option<&str>) {
    if let Some(turn_id) = turn_id {
        tracing::info!(thread_id, turn_id, "requesting internal turn interruption");
        match request_interrupt(runtime, thread_id, turn_id).await {
            Ok(_) => tracing::info!(
                turn_id,
                "internal interruption accepted; completion not yet confirmed"
            ),
            Err(error) => {
                tracing::warn!(turn_id, %error, "internal interruption result is unknown")
            }
        }
    }
}

// Filter before the caller's timeout is reset: traffic from other turns
// must not keep an unresponsive turn alive.
async fn recv_turn_event(
    notifications: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    turn_id: Option<&str>,
) -> Result<Value, broadcast::error::RecvError> {
    loop {
        let event = notifications.recv().await?;
        if event.get("kind").and_then(Value::as_str) == Some("transport_closed") {
            return Err(broadcast::error::RecvError::Closed);
        }
        if matches_thread_and_turn(&event, thread_id, turn_id)
            || (interaction_error(&event, thread_id).is_some()
                && event["turnId"].as_str().is_none())
            || event
                .get("params")
                .is_some_and(|params| matches_thread_and_turn(params, thread_id, turn_id))
        {
            return Ok(event);
        }
    }
}

fn matches_thread_and_turn(params: &Value, thread_id: &str, turn_id: Option<&str>) -> bool {
    params.get("threadId").and_then(Value::as_str) == Some(thread_id)
        && turn_id.is_none_or(|expected| {
            params.get("turnId").and_then(Value::as_str) == Some(expected)
                || params.pointer("/turn/id").and_then(Value::as_str) == Some(expected)
        })
}

fn is_thread_not_found(error: &RuntimeError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("thread")
        && (message.contains("not found") || message.contains("unknown thread"))
}

fn is_approval_required(event: &Value, thread_id: &str) -> bool {
    event.get("kind").and_then(Value::as_str) == Some("approval_required")
        && event.get("threadId").and_then(Value::as_str) == Some(thread_id)
}

pub(crate) fn interaction_error(event: &Value, thread_id: &str) -> Option<&'static str> {
    if is_approval_required(event, thread_id) {
        Some("approval_required")
    } else if event["kind"] == "interaction_unavailable" && event["threadId"] == thread_id {
        Some("unsupported_interaction")
    } else {
        None
    }
}

fn matches_turn_event(event: &Value, turn_id: &str) -> bool {
    if event.get("turnId").and_then(Value::as_str) == Some(turn_id) {
        return true;
    }
    event
        .get("params")
        .is_some_and(|params| params.get("turnId").and_then(Value::as_str) == Some(turn_id))
}

fn collect_text(value: &Value) -> String {
    let mut result = String::new();
    collect_text_values(value, &mut result);
    result
}

fn collect_text_values(value: &Value, result: &mut String) {
    match value {
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                result.push_str(text);
            }
            for child in map.values() {
                collect_text_values(child, result);
            }
        }
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_text_values(value, result)),
        _ => {}
    }
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

pub(crate) fn reasoning_name(value: crate::model::ReasoningEffort) -> &'static str {
    match value {
        crate::model::ReasoningEffort::None => "none",
        crate::model::ReasoningEffort::Low => "low",
        crate::model::ReasoningEffort::Medium => "medium",
        crate::model::ReasoningEffort::High => "high",
        crate::model::ReasoningEffort::XHigh => "xhigh",
        crate::model::ReasoningEffort::Max => "max",
    }
}

fn model_error(error: ModelError) -> ApiError {
    match error {
        ModelError::NotFound(message) => {
            ApiError::new(StatusCode::NOT_FOUND, "model_not_found", message)
        }
        ModelError::ProviderUnavailable(message) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_unavailable",
            message,
        ),
        ModelError::ProviderAuthenticationRequired(message) => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "provider_authentication_required",
            message,
        ),
        ModelError::UnsupportedReasoning(message) | ModelError::UnsupportedEffort(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, "unsupported_parameter", message)
        }
        ModelError::InvalidRegistry(message) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            message,
        ),
    }
}

fn approval_error(error: crate::approval_manager::ApprovalManagerError) -> ApiError {
    match error {
        crate::approval_manager::ApprovalManagerError::NotFound(_) => ApiError::new(
            StatusCode::NOT_FOUND,
            "approval_not_found",
            error.to_string(),
        ),
        crate::approval_manager::ApprovalManagerError::InvalidDecision(_)
        | crate::approval_manager::ApprovalManagerError::Rejected => {
            ApiError::new(StatusCode::CONFLICT, "approval_rejected", error.to_string())
        }
        crate::approval_manager::ApprovalManagerError::Runtime(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "runtime_error", error.to_string())
        }
    }
}

fn runtime_error(error: RuntimeError) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "runtime_error", error.to_string())
}

fn turn_failure_message(status: &str, params: &Value) -> String {
    let detail = params
        .pointer("/turn/error")
        .or_else(|| params.pointer("/turn/lastError"))
        .or_else(|| params.get("error"))
        .filter(|value| !value.is_null())
        .map(Value::to_string)
        .unwrap_or_default();
    if detail.is_empty() {
        format!("Codex turn ended with status {status}")
    } else {
        format!("Codex turn ended with status {status}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_required_is_scoped_to_thread() {
        let event = json!({
            "kind": "approval_required",
            "threadId": "thread_1",
            "approval_id": "approval_1"
        });
        assert!(is_approval_required(&event, "thread_1"));
        assert!(!is_approval_required(&event, "thread_2"));
        assert!(!is_approval_required(
            &json!({"kind": "approval_requested", "threadId": "thread_1"}),
            "thread_1"
        ));
    }

    #[test]
    fn turn_event_filter_accepts_proxy_and_codex_event_shapes() {
        assert!(matches_turn_event(
            &json!({"kind": "approval_resolved", "turnId": "turn_1"}),
            "turn_1"
        ));
        assert!(matches_turn_event(
            &json!({
                "method": "turn/completed",
                "params": {"turnId": "turn_1"}
            }),
            "turn_1"
        ));
        assert!(!matches_turn_event(
            &json!({"kind": "approval_resolved", "turnId": "turn_2"}),
            "turn_1"
        ));
        assert!(!matches_turn_event(
            &json!({"kind": "approval_resolved", "threadId": "thread_1"}),
            "turn_1"
        ));
    }

    #[test]
    fn chat_messages_are_projected_to_role_tagged_text() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: Value::String("Be concise".into()),
            },
            ChatMessage {
                role: "user".into(),
                content: Value::String("Hello".into()),
            },
        ];
        let input = chat_messages_to_input(&messages).unwrap();
        assert_eq!(input[0]["text"], "[system]\nBe concise");
        assert_eq!(input[1]["text"], "[user]\nHello");
    }

    #[test]
    fn chat_messages_reject_unsupported_content() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([{ "type": "input_audio", "data": "unsupported" }]),
        }];
        assert_eq!(
            chat_messages_to_input(&messages).unwrap_err().code,
            "unsupported_parameter"
        );
    }

    #[test]
    fn turn_failure_message_preserves_codex_error_detail() {
        let message = turn_failure_message(
            "failed",
            &json!({"turn": {"error": {"message": "model rejected request"}}}),
        );
        assert_eq!(
            message,
            "Codex turn ended with status failed: {\"message\":\"model rejected request\"}"
        );
        assert_eq!(
            turn_failure_message("interrupted", &json!({})),
            "Codex turn ended with status interrupted"
        );
    }
}

fn control_store_error(error: std::io::Error) -> ApiError {
    tracing::error!(%error, "control store unavailable");
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_store_unavailable",
        "durable control store unavailable; do not retry execution",
    )
}

fn validate_request_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_id",
            "request ID must be 1..128 ASCII letters, digits, -_.",
        ));
    }
    Ok(())
}

fn add_identity_headers(response: &mut Response, record: &Value) {
    for (header, field) in [
        ("x-response-id", "response_id"),
        ("x-codex-thread-id", "thread_id"),
        ("x-codex-turn-id", "turn_id"),
    ] {
        if let Some(value) = record
            .get(field)
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok())
        {
            response.headers_mut().insert(header, value);
        }
    }
}

async fn capabilities(State(state): State<AppState>) -> Json<Value> {
    state.catalog.refresh().await;
    let providers = state.catalog.provider_statuses().await;
    Json(
        json!({"contract_version":"1.0", "responses":true, "streaming":true,
        "conversation_resume":true, "conversation_model_change":true, "identity_on_start":true, "request_lookup":true,
        "persistent_turn_status":true, "turn_status":true, "turn_events":true,
        "turn_interrupt":true, "turn_steer":true, "interactive_approval":true,
        "auto_approval_suppression":true, "event_reconnect":true, "output_retrieval":false,
        "limits":{"auth_scope":"shared_operator", "request_retention":"no_automatic_deletion",
            "event_reconnect":"snapshot_only", "event_history_replay":false,
            "steer_idempotency":false, "disconnect_interrupts":true,
            "approval_kinds":["commandExecution","fileChange"], "user_input":false, "mcp_elicitation":false, "permissions_approval":false,
            "model_change_scope":"same_provider", "continuation":"successful_response_only"},
        "providers":providers}),
    )
}

async fn get_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_request_id(&id)?;
    let record = state
        .responses
        .control
        .by_request(&id)
        .map_err(control_store_error)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "request_not_found",
                "no retained record; this does not prove that the request was never sent",
            )
        })?;
    Ok(Json(record.public()))
}
async fn get_execution(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let record = state
        .responses
        .control
        .get(&id)
        .map_err(control_store_error)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "response_not_found",
                "execution metadata not found (legacy mappings have no turn identity)",
            )
        })?;
    let mut value = record.public();
    value["continuable"] = json!(
        record.last_observed_status == "completed" && state.responses.get(&id).await.is_some()
    );
    Ok(Json(value))
}
fn execution_for_turn(state: &AppState, id: &str) -> Result<Execution, ApiError> {
    state
        .responses
        .control
        .by_turn(id)
        .map_err(control_store_error)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "turn_not_found",
                "turn was not recorded by this proxy",
            )
        })
}
async fn turn_approvals(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    execution_for_turn(&state, &id)?;
    Ok(Json(
        json!({"turn_id":id, "data":state.approvals.pending_events_for_turn(&id).await}),
    ))
}

async fn lock_thread(
    state: &AppState,
    thread: &str,
) -> Result<tokio::sync::OwnedMutexGuard<()>, ApiError> {
    let lock = state
        .thread_locks
        .lock()
        .await
        .entry(thread.into())
        .or_default()
        .clone();
    lock.try_lock_owned().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "thread_busy",
            "another request owns this thread",
        )
    })
}
async fn check_thread_idle(state: &AppState, thread: &str) -> Result<(), ApiError> {
    for record in state
        .responses
        .control
        .records()
        .map_err(control_store_error)?
    {
        if record.thread_id.as_deref() != Some(thread)
            || terminal(&record.last_observed_status)
            || record.phase == "rejected"
        {
            continue;
        }
        let Some(turn_id) = record.turn_id else {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "thread_state_unknown",
                "previous start outcome is unknown",
            ));
        };
        let snapshot = turn_snapshot(state, &turn_id).await?;
        if !snapshot["status"].as_str().is_some_and(terminal) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "thread_busy",
                "previous turn has not been confirmed terminal",
            ));
        }
    }
    Ok(())
}

async fn turn_snapshot(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let record = execution_for_turn(state, id)?;
    let result = state
        .runtime
        .request(
            "thread/read",
            json!({"threadId": record.thread_id, "includeTurns":true}),
        )
        .await;
    let queried_at = now_ms();
    let turn = result
        .as_ref()
        .ok()
        .and_then(|v| v.pointer("/thread/turns").or_else(|| v.get("turns")))
        .and_then(Value::as_array)
        .and_then(|turns| turns.iter().find(|t| t["id"].as_str() == Some(id)))
        .cloned();
    let status = turn
        .as_ref()
        .and_then(|t| t["status"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    if status != "unknown" {
        state
            .responses
            .control
            .observe(id, &status)
            .map_err(control_store_error)?;
    }
    if terminal(&status) {
        state
            .approvals
            .invalidate_turn(record.thread_id.as_deref().unwrap_or_default(), id)
            .await;
    }
    let latest = execution_for_turn(state, id)?;
    Ok(
        json!({"object":"codex.turn_status", "turn_id":id, "response_id":record.response_id,
        "thread_id":record.thread_id, "model":record.model_id, "status":status,
        "last_observed_status":latest.last_observed_status, "last_observed_at_ms":latest.last_observed_at_ms,
        "last_event_at_ms":latest.last_observed_at_ms, "started_at_ms":record.started_at_ms, "queried_at_ms":queried_at,
        "turn":turn, "thread_status":result.as_ref().ok().and_then(|v|v.pointer("/thread/status")).cloned(),
        "runtime_query_error":result.err().map(|e| e.to_string()),
        "pending_approvals":state.approvals.pending_events_for_turn(id).await,
        "output_retrieval":"unavailable"}),
    )
}

async fn control_interrupt(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let target = execution_for_turn(&state, &id)?;
    let lock = state
        .control_locks
        .lock()
        .await
        .entry(target.thread_id.clone().expect("recorded turn has thread"))
        .or_default()
        .clone();
    let _guard = lock.lock().await;
    let snapshot = turn_snapshot(&state, &id).await?;
    if snapshot["status"].as_str().is_some_and(terminal) {
        return Ok(Json(
            json!({"turn_id":id, "result":"already_terminal", "status":snapshot["status"]}),
        )
        .into_response());
    }
    let record = execution_for_turn(&state, &id)?;
    if let Some(previous) = record.interrupt_state {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"turn_id":id,"result":previous,"status":snapshot["status"]})),
        )
            .into_response());
    }
    if snapshot["status"] != "inProgress" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "turn_state_unknown",
            "cannot confirm an active turn",
        ));
    }
    state
        .responses
        .control
        .update(&record.response_id, |r| {
            r.interrupt_state = Some("unknown".into())
        })
        .map_err(control_store_error)?;
    let result = request_interrupt(
        &state.runtime,
        record.thread_id.as_deref().expect("recorded thread"),
        &id,
    )
    .await;
    match result {
        Ok(_) => {
            state
                .responses
                .control
                .update(&record.response_id, |r| {
                    r.interrupt_state = Some("accepted".into())
                })
                .map_err(control_store_error)?;
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({"turn_id":id,"result":"accepted","status":"not_yet_confirmed"})),
            )
                .into_response())
        }
        Err(error) => Err(control_runtime_error(error)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SteerRequest {
    expected_turn_id: String,
    input: Value,
}
async fn control_steer(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SteerRequest>,
) -> Result<Response, ApiError> {
    if id != request.expected_turn_id {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "turn_mismatch",
            "expected_turn_id must match URL turn",
        ));
    }
    let input = normalize_input(&request.input)?;
    if input.is_empty()
        || input.iter().all(|part| {
            part["type"] == "text"
                && part["text"]
                    .as_str()
                    .is_some_and(|text| text.trim().is_empty())
        })
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "Steer input must not be empty",
        ));
    }
    let target = execution_for_turn(&state, &id)?;
    let lock = state
        .control_locks
        .lock()
        .await
        .entry(target.thread_id.clone().expect("recorded turn has thread"))
        .or_default()
        .clone();
    let _guard = lock.lock().await;
    let snapshot = turn_snapshot(&state, &id).await?;
    let record = execution_for_turn(&state, &id)?;
    let waiting_for_input = snapshot
        .pointer("/thread_status/activeFlags")
        .and_then(Value::as_array)
        .is_some_and(|flags| {
            flags.iter().any(|flag| {
                matches!(
                    flag.as_str(),
                    Some("waitingOnApproval" | "waitingOnUserInput")
                )
            })
        });
    if snapshot["status"] != "inProgress"
        || waiting_for_input
        || record.interrupt_state.is_some()
        || !state
            .approvals
            .pending_events_for_turn(&id)
            .await
            .is_empty()
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "turn_not_steerable",
            "turn is inactive, unknown, awaiting approval, or interrupt requested",
        ));
    }
    let params = json!({"threadId":record.thread_id,"expectedTurnId":id,"input":input});
    let result = state
        .runtime
        .request("turn/steer", params)
        .await
        .map_err(control_runtime_error)?;
    if result["turnId"].as_str() != Some(&id) {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "steer_result_unknown",
            "upstream did not confirm expected turn; do not resend",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"turn_id":id,"result":"accepted"})),
    )
        .into_response())
}

fn control_runtime_error(error: RuntimeError) -> ApiError {
    if matches!(&error, RuntimeError::Rpc { code: -32602, .. }) {
        ApiError::new(
            StatusCode::CONFLICT,
            "upstream_control_rejected",
            error.to_string(),
        )
    } else {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "control_result_unknown",
            format!("{error}; do not automatically resend"),
        )
    }
}

fn start_error(
    state: &AppState,
    response_id: &str,
    error: RuntimeError,
    resuming: bool,
) -> ApiError {
    if matches!(&error, RuntimeError::Rpc { code: -32602, .. })
        && let Err(error) = state
            .responses
            .control
            .update(response_id, |r| r.phase = "rejected".into())
    {
        return control_store_error(error);
    }
    if resuming && is_thread_not_found(&error) {
        ApiError::new(StatusCode::NOT_FOUND, "thread_not_found", error.to_string())
    } else {
        runtime_error(error)
    }
}
