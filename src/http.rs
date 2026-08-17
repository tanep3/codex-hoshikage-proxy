use crate::{
    config::CwdPolicy,
    journal::{EventJournal, JournalEntry, now_ms},
    model::{ModelError, ModelRegistry},
    runtime::{CodexRuntime, RuntimeError},
    store::{ResponseMapping, ResponseStore},
    turn::{ResponseContentPart, ResponseOutputItem, ResponseRecord},
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
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
    pub journal: Arc<EventJournal>,
    responses: Arc<ResponseStore>,
}

struct StartedTurn {
    model: crate::model::ResolvedModel,
    thread_id: String,
    turn_id: Option<String>,
    notifications: broadcast::Receiver<Value>,
}

impl AppState {
    pub fn new(
        runtime: Arc<CodexRuntime>,
        models: ModelRegistry,
        cwd_policy: CwdPolicy,
        default_cwd: std::path::PathBuf,
        journal: Arc<EventJournal>,
        responses: Arc<ResponseStore>,
    ) -> Self {
        Self {
            runtime,
            models: Arc::new(models),
            cwd_policy,
            default_cwd,
            journal,
            responses,
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
        .route("/v1/responses", post(create_response))
        .with_state(state)
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

async fn begin_turn(state: &AppState, request: &ResponsesRequest) -> Result<StartedTurn, ApiError> {
    let reasoning = request
        .reasoning
        .as_ref()
        .and_then(|value| value.effort.as_deref());
    let model = state
        .models
        .resolve(request.model.as_deref(), reasoning)
        .map_err(model_error)?;
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
                    "ephemeral": false,
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

fn runtime_error(error: RuntimeError) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "runtime_error", error.to_string())
}
