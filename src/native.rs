use axum::{
    extract::ws::{CloseFrame, Message, WebSocket},
    http::StatusCode,
};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore},
};

#[derive(Debug, Clone)]
pub struct NativeConfig {
    pub command: String,
    pub args: Vec<String>,
    pub codex_home: PathBuf,
    pub max_message_bytes: usize,
    pub shutdown_grace: Duration,
}

#[derive(Debug)]
pub struct NativeBridge {
    config: NativeConfig,
    connections: Arc<Semaphore>,
}

impl NativeBridge {
    pub fn new(config: NativeConfig, max_connections: usize) -> Self {
        Self {
            config,
            connections: Arc::new(Semaphore::new(max_connections)),
        }
    }

    pub fn max_message_bytes(&self) -> usize {
        self.config.max_message_bytes
    }

    pub fn try_acquire(&self) -> Result<OwnedSemaphorePermit, StatusCode> {
        self.connections
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }

    pub async fn serve(self: Arc<Self>, mut socket: WebSocket, _permit: OwnedSemaphorePermit) {
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .env("CODEX_HOME", &self.config.codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::error!(%error, "failed to start native Codex App Server");
                close(&mut socket, 1011, "Codex App Server could not start").await;
                return;
            }
        };
        let Some(mut stdin) = child.stdin.take() else {
            close(&mut socket, 1011, "Codex stdin unavailable").await;
            let _ = child.kill().await;
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            close(&mut socket, 1011, "Codex stdout unavailable").await;
            let _ = child.kill().await;
            return;
        };
        let mut stdout = BufReader::new(stdout);
        let mut upstream = Vec::new();

        loop {
            tokio::select! {
                message = socket.recv() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            if text.len() > self.config.max_message_bytes {
                                close(&mut socket, 1009, "message too large").await;
                                break;
                            }
                            if serde_json::from_str::<Value>(&text).is_err() {
                                close(&mut socket, 1007, "invalid JSON").await;
                                break;
                            }
                            if stdin.write_all(text.as_bytes()).await.is_err()
                                || stdin.write_all(b"\n").await.is_err()
                                || stdin.flush().await.is_err()
                            {
                                close(&mut socket, 1011, "Codex stdin closed").await;
                                break;
                            }
                        }
                        Some(Ok(Message::Binary(_))) => {
                            close(&mut socket, 1003, "binary messages are not supported").await;
                            break;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            if socket.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(error)) => {
                            tracing::debug!(%error, "native WebSocket receive failed");
                            break;
                        }
                    }
                }
                read = stdout.read_until(b'\n', &mut upstream) => {
                    match read {
                        Ok(0) => {
                            close(&mut socket, 1011, "Codex App Server exited").await;
                            break;
                        }
                        Ok(_) => {
                            if upstream.len() > self.config.max_message_bytes + 1 {
                                close(&mut socket, 1009, "upstream message too large").await;
                                break;
                            }
                            while upstream.last().is_some_and(|byte| matches!(byte, b'\n' | b'\r')) {
                                upstream.pop();
                            }
                            let text = match std::str::from_utf8(&upstream) {
                                Ok(text) if serde_json::from_str::<Value>(text).is_ok() => text,
                                _ => {
                                    close(&mut socket, 1007, "invalid upstream JSON").await;
                                    break;
                                }
                            };
                            if socket.send(Message::Text(text.to_owned().into())).await.is_err() {
                                break;
                            }
                            upstream.clear();
                        }
                        Err(error) => {
                            tracing::debug!(%error, "native Codex stdout failed");
                            close(&mut socket, 1011, "Codex stdout failed").await;
                            break;
                        }
                    }
                }
            }
        }

        drop(stdin);
        match tokio::time::timeout(self.config.shutdown_grace, child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}
