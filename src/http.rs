use crate::{
    approval_manager::{ApprovalCapability, ApprovalDecisionRequest, ApprovalManager},
    config::CwdPolicy,
    journal::{EventJournal, JournalEntry, now_ms},
    model::{ModelError, ModelRegistry},
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
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc},
    time,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<CodexRuntime>,
    pub models: Arc<ModelRegistry>,
    pub cwd_policy: CwdPolicy,
    pub default_cwd: std::path::PathBuf,
    pub api_key: Option<String>,
    pub journal: Arc<EventJournal>,
    responses: Arc<ResponseStore>,
    permits: Arc<ProviderPermitPool>,
    pub approvals: Arc<ApprovalManager>,
    next_chat_id: Arc<AtomicU64>,
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
        models: ModelRegistry,
        cwd_policy: CwdPolicy,
        default_cwd: std::path::PathBuf,
        api_key: Option<String>,
        approval_timeout: Duration,
        journal: Arc<EventJournal>,
        responses: Arc<ResponseStore>,
    ) -> Self {
        let provider_limits = models.provider_limits();
        let approvals = ApprovalManager::new(Arc::clone(&runtime), approval_timeout);
        approvals.start();
        Self {
            runtime,
            models: Arc::new(models),
            cwd_policy,
            default_cwd,
            api_key,
            journal,
            responses,
            permits: Arc::new(ProviderPermitPool::new(provider_limits)),
            approvals,
            next_chat_id: Arc::new(AtomicU64::new(1)),
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
        "data": state.models.list_public_models(),
    }))
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
    tokio::spawn(async move {
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
        let event = tokio::time::timeout(Duration::from_secs(120), notifications.recv())
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
    let text = collect_turn_text(&mut notifications, &thread_id, turn_id.as_deref()).await?;
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
) -> Result<String, ApiError> {
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(120), notifications.recv())
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
        let event = time::timeout(Duration::from_secs(120), notifications.recv()).await;
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
                        &json!({"error": format!("Codex turn ended with status {status}")}),
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
        .models
        .resolve(request.model.as_deref(), reasoning)
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
    state.approvals.register_turn(&thread_id, capability).await;
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
    loop {
        let event = time::timeout(Duration::from_secs(120), notifications.recv()).await;
        let Ok(Ok(event)) = event else {
            let _ = sender
                .send(
                    sse_json(
                        "response.failed",
                        &json!({"id": response_id, "status": "failed"}),
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
}
