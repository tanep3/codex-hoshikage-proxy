use crate::{
    approval_manager::{ApprovalCapability, ApprovalDecisionRequest, ApprovalManager},
    catalog::ModelCatalogManager,
    config::CwdPolicy,
    journal::{EventJournal, JournalEntry, now_ms},
    model::ModelError,
    permit::ProviderPermitPool,
    runtime::{CodexRuntime, RuntimeError},
    store::{ResponseMapping, ResponseStore},
    turn::{ResponseContentPart, ResponseOutputItem, ResponseRecord},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{RwLock, broadcast, mpsc},
    time,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<CodexRuntime>,
    pub catalog: Arc<ModelCatalogManager>,
    pub cwd_policy: CwdPolicy,
    pub default_cwd: std::path::PathBuf,
    pub api_key: Option<String>,
    pub turn_idle_timeout: Duration,
    pub turn_stall_detection: Duration,
    tracked_turns: Arc<RwLock<HashMap<String, TrackedTurn>>>,
    pub journal: Arc<EventJournal>,
    responses: Arc<ResponseStore>,
    permits: Arc<ProviderPermitPool>,
    pub approvals: Arc<ApprovalManager>,
    next_chat_id: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct TrackedTurn {
    thread_id: String,
    model_id: String,
    started_at_ms: u128,
    last_event_at_ms: u128,
    last_status: String,
}

struct StartedTurn {
    model: crate::model::ResolvedModel,
    thread_id: String,
    turn_id: Option<String>,
    notifications: broadcast::Receiver<Value>,
    _permit: tokio::sync::OwnedSemaphorePermit,
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
        approval_timeout: Duration,
        auto_approve_workspace: bool,
        journal: Arc<EventJournal>,
        responses: Arc<ResponseStore>,
    ) -> Self {
        let provider_limits = catalog.provider_limits();
        let approvals = ApprovalManager::new(
            Arc::clone(&runtime),
            approval_timeout,
            auto_approve_workspace,
        );
        approvals.start();
        Self {
            runtime,
            catalog: Arc::new(catalog),
            cwd_policy,
            default_cwd,
            api_key,
            turn_idle_timeout,
            turn_stall_detection,
            tracked_turns: Arc::new(RwLock::new(HashMap::new())),
            journal,
            responses,
            permits: Arc::new(ProviderPermitPool::new(provider_limits)),
            approvals,
            next_chat_id: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn track_turn(&self, turn_id: &str, thread_id: &str, model_id: &str) {
        let now = now_ms();
        self.tracked_turns.write().await.insert(
            turn_id.to_owned(),
            TrackedTurn {
                thread_id: thread_id.to_owned(),
                model_id: model_id.to_owned(),
                started_at_ms: now,
                last_event_at_ms: now,
                last_status: "inProgress".into(),
            },
        );
    }

    async fn touch_turn(&self, turn_id: &str, status: Option<&str>) {
        let mut turns = self.tracked_turns.write().await;
        if let Some(turn) = turns.get_mut(turn_id) {
            turn.last_event_at_ms = now_ms();
            if let Some(status) = status {
                turn.last_status = status.to_owned();
            }
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
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
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
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
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
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/models", get(list_models))
        .route("/v1/responses", post(create_response))
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
        .is_some_and(|value| value == expected);
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

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": state.catalog.list_public_models().await,
    }))
}

async fn get_turn_status(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let tracked = state
        .tracked_turns
        .read()
        .await
        .get(&turn_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "turn_not_found",
                format!("turn not found: {turn_id}"),
            )
        })?;
    let runtime_status = state
        .runtime
        .request(
            "thread/read",
            json!({"threadId": tracked.thread_id, "includeTurns": true}),
        )
        .await;
    let turn = runtime_status.as_ref().ok().and_then(|value| {
        value
            .pointer("/thread/turns")
            .or_else(|| value.get("turns"))
            .and_then(Value::as_array)
            .and_then(|turns| {
                turns
                    .iter()
                    .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id.as_str()))
            })
            .cloned()
    });
    let codex_thread_status = runtime_status.as_ref().ok().and_then(|value| {
        value
            .pointer("/thread/status")
            .or_else(|| value.get("status"))
            .cloned()
    });
    let status = turn
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or(&tracked.last_status);
    Ok(Json(json!({
        "object": "codex.turn_status",
        "turn_id": turn_id,
        "thread_id": tracked.thread_id,
        "model": tracked.model_id,
        "status": status,
        "started_at_ms": tracked.started_at_ms,
        "last_event_at_ms": tracked.last_event_at_ms,
        "thread_status": codex_thread_status,
        "turn": turn,
        "runtime_query_error": runtime_status.err().map(|error| error.to_string()),
    })))
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
) -> Response {
    let mut notifications = state.runtime.subscribe();
    let (sender, receiver) = mpsc::channel::<Event>(32);
    let approvals = Arc::clone(&state.approvals);
    tokio::spawn(async move {
        for event in approvals.pending_events_for_turn(&turn_id).await {
            if sender
                .send(
                    Event::default()
                        .event("approval_requested")
                        .json_data(&event)
                        .unwrap_or_default(),
                )
                .await
                .is_err()
            {
                return;
            }
        }
        loop {
            let event = match notifications.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if !matches_turn_event(&event, &turn_id) {
                continue;
            }
            let event_name = event
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| event.get("method").and_then(Value::as_str))
                .unwrap_or("codex.event");
            if sender
                .send(
                    Event::default()
                        .event(event_name)
                        .json_data(&event)
                        .unwrap_or_default(),
                )
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Sse::new(ReceiverStream::new(receiver).map(Ok::<Event, std::convert::Infallible>))
        .into_response()
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.runtime.snapshot().await == crate::domain::RuntimeState::Ready {
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
    Json(request): Json<ResponsesRequest>,
) -> Result<Response, ApiError> {
    let response_id = state.responses.next_response_id();
    let started = begin_turn(&state, &request).await?;
    if request.stream {
        return stream_response(state, response_id, started).await;
    }
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
    } = started;
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(state.turn_idle_timeout, notifications.recv())
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "runtime_timeout",
                    "Codex turn timed out",
                )
            })?
            .map_err(|error| runtime_error(RuntimeError::Protocol(error.to_string())))?;
        if is_approval_required(&event, &thread_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "approval_required",
                "client does not provide interactive approval capability",
            ));
        }
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").cloned().unwrap_or_else(|| json!({}));
        if !matches_thread_and_turn(&params, &thread_id, turn_id.as_deref()) {
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
                break;
            }
            _ => {}
        }
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
    Ok((StatusCode::OK, Json(response)).into_response())
}

async fn create_chat_completion(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionsRequest>,
) -> Result<Response, ApiError> {
    let input = chat_messages_to_input(&request.messages)?;
    let internal = ResponsesRequest {
        model: request.model.clone(),
        input: Value::Array(input),
        previous_response_id: None,
        stream: request.stream,
        metadata: request.metadata,
        reasoning: None,
    };
    let started = begin_turn_with_mode(&state, &internal, true).await?;
    if request.stream {
        return stream_chat_completion(state, started).await;
    }
    let StartedTurn {
        model,
        thread_id,
        turn_id,
        mut notifications,
        _permit,
    } = started;
    let text = collect_turn_text(
        &mut notifications,
        &thread_id,
        turn_id.as_deref(),
        state.turn_idle_timeout,
    )
    .await?;
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
    notifications: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    turn_id: Option<&str>,
    idle_timeout: Duration,
) -> Result<String, ApiError> {
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(idle_timeout, notifications.recv())
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "runtime_timeout",
                    "Codex turn timed out",
                )
            })?
            .map_err(|error| runtime_error(RuntimeError::Protocol(error.to_string())))?;
        if is_approval_required(&event, thread_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "approval_required",
                "client does not provide interactive approval capability",
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
        let event = time::timeout(state.turn_idle_timeout, notifications.recv()).await;
        let Ok(Ok(event)) = event else {
            let _ = sender
                .send(sse_data(&json!({"error": "Codex turn timed out"})))
                .await;
            let _ = sender.send(sse_done()).await;
            return;
        };
        if is_approval_required(&event, &thread_id) {
            let _ = sender
                .send(sse_data(&json!({"error": {"code": "approval_required"}})))
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
    messages
        .iter()
        .map(|message| {
            let content = message.content.as_str().ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "only string message content is supported",
                )
            })?;
            Ok(json!({"type": "text", "text": format!("[{}]\n{}", message.role, content)}))
        })
        .collect()
}

async fn begin_turn(state: &AppState, request: &ResponsesRequest) -> Result<StartedTurn, ApiError> {
    begin_turn_with_mode(state, request, false).await
}

async fn begin_turn_with_mode(
    state: &AppState,
    request: &ResponsesRequest,
    ephemeral: bool,
) -> Result<StartedTurn, ApiError> {
    let reasoning = request
        .reasoning
        .as_ref()
        .and_then(|value| value.effort.as_deref());
    let model = state
        .catalog
        .resolve(request.model.as_deref(), reasoning)
        .await
        .map_err(model_error)?;
    let permit = state
        .permits
        .acquire(&model.public_provider_id)
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unavailable",
                error.to_string(),
            )
        })?;
    let cwd = request
        .metadata
        .get("codex.cwd")
        .map(String::as_str)
        .map(|value| state.cwd_policy.validate(value))
        .transpose()
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, "invalid_cwd", error.to_string()))?
        .unwrap_or_else(|| state.default_cwd.clone());
    let input = normalize_input(&request.input)?;
    let resuming = request.previous_response_id.is_some();
    let thread_id = if let Some(response_id) = request.previous_response_id.as_deref() {
        let context = state.responses.get(response_id).await.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "thread_not_found",
                "previous response was not found",
            )
        })?;
        if context.model_id != model.public_model_id {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "model_mismatch",
                "a durable Responses thread cannot change model",
            ));
        }
        context.thread_id
    } else {
        let result = state
            .runtime
            .request(
                "thread/start",
                json!({
                    "model": model.upstream_model_id,
                    "modelProvider": model.codex_provider_id,
                    "cwd": cwd,
                    "ephemeral": ephemeral,
                    "approvalPolicy": "on-request",
                    "sandbox": "workspace-write"
                }),
            )
            .await
            .map_err(runtime_error)?;
        string_at(&result, &["thread", "id"])
            .or_else(|| result.get("id").and_then(Value::as_str))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "runtime_error",
                    "thread/start returned no thread id",
                )
            })?
            .to_string()
    };
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
        .register_turn(&thread_id, capability, &cwd)
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
                "approvalPolicy": "on-request"
            }),
        )
        .await
        .map_err(|error| {
            if resuming && is_thread_not_found(&error) {
                ApiError::new(StatusCode::NOT_FOUND, "thread_not_found", error.to_string())
            } else {
                runtime_error(error)
            }
        })?;
    let turn_id = string_at(&turn_result, &["turn", "id"])
        .or_else(|| turn_result.get("id").and_then(Value::as_str))
        .map(str::to_owned);
    if let Some(turn_id) = turn_id.as_deref() {
        state
            .track_turn(turn_id, &thread_id, &model.public_model_id)
            .await;
    }
    Ok(StartedTurn {
        model,
        thread_id,
        turn_id,
        notifications,
        _permit: permit,
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
    tokio::spawn(run_stream(state, response_id, started, sender));
    let stream = ReceiverStream::new(receiver).map(Ok::<Event, std::convert::Infallible>);
    Ok(Sse::new(stream).into_response())
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
    } = started;
    let mut silent_since = Instant::now();
    loop {
        let remaining = state
            .turn_idle_timeout
            .checked_sub(silent_since.elapsed())
            .unwrap_or_default();
        let probe_after = remaining.min(state.turn_stall_detection);
        let event = time::timeout(probe_after, notifications.recv()).await;
        let event = match event {
            Ok(Ok(event)) => {
                silent_since = Instant::now();
                event
            }
            Err(_) => {
                if silent_since.elapsed() < state.turn_idle_timeout {
                    match probe_turn(&state, &thread_id, turn_id.as_deref()).await {
                        Ok(probe) if probe.waiting_for_user => {
                            tracing::info!(
                                response_id = %response_id,
                                thread_id = %thread_id,
                                turn_id = ?turn_id,
                                "Codex Turn is waiting for user input; watchdog remains active"
                            );
                            continue;
                        }
                        Ok(probe) if probe.status == "inProgress" => {
                            let message = format!(
                                "Codex Turn remained inProgress without events for {} seconds",
                                silent_since.elapsed().as_secs()
                            );
                            fail_response_stream(
                                &state,
                                &sender,
                                &response_id,
                                &model.public_model_id,
                                &thread_id,
                                turn_id.as_deref(),
                                "turn_stalled",
                                message,
                            )
                            .await;
                            break;
                        }
                        Ok(probe) => {
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
        if is_approval_required(&event, &thread_id) {
            let _ = sender
                .send(
                    sse_json(
                        "response.failed",
                        &json!({
                            "id": response_id,
                            "status": "failed",
                            "error": {"code": "approval_required"}
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
    thread_id: &str,
    turn_id: Option<&str>,
    code: &str,
    message: String,
) {
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

fn normalize_input(input: &Value) -> Result<Vec<Value>, ApiError> {
    if let Some(text) = input.as_str() {
        return Ok(vec![json!({ "type": "text", "text": text })]);
    }
    let items = input.as_array().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "input must be a string or array",
        )
    })?;
    items
        .iter()
        .map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("input_text") {
                Ok(json!({ "type": "text", "text": item.get("text").and_then(Value::as_str).unwrap_or_default() }))
            } else if item.get("type").and_then(Value::as_str) == Some("text") {
                Ok(item.clone())
            } else {
                Err(ApiError::new(StatusCode::BAD_REQUEST, "unsupported_parameter", "only text input is supported in Phase 2"))
            }
        })
        .collect()
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

fn reasoning_name(value: crate::model::ReasoningEffort) -> &'static str {
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
    fn chat_messages_reject_non_string_content() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([{ "type": "text", "text": "Hello" }]),
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
