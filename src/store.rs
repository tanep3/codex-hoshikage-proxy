use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

#[derive(Debug, Error)]
pub enum ResponseStoreError {
    #[error("failed to prepare response store: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to decode response store entry: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMapping {
    pub response_id: String,
    pub thread_id: String,
    pub model_id: String,
}

pub struct ResponseStore {
    path: PathBuf,
    writer: Mutex<tokio::fs::File>,
    mappings: Mutex<HashMap<String, ResponseMapping>>,
    next_id: AtomicU64,
}

impl ResponseStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, ResponseStoreError> {
        let directory = root.as_ref().join("state/responses");
        fs::create_dir_all(&directory).await?;
        let path = directory.join("mappings.jsonl");
        let mut mappings = HashMap::new();
        let mut next_id = 1;
        match fs::read_to_string(&path).await {
            Ok(contents) => {
                for line in contents.lines().filter(|line| !line.trim().is_empty()) {
                    let mapping: ResponseMapping = serde_json::from_str(line)?;
                    next_id =
                        next_id.max(parse_response_id(&mapping.response_id).saturating_add(1));
                    mappings.insert(mapping.response_id.clone(), mapping);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            writer: Mutex::new(writer),
            mappings: Mutex::new(mappings),
            next_id: AtomicU64::new(next_id),
        })
    }

    pub fn next_response_id(&self) -> String {
        format!("resp_{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    pub async fn get(&self, response_id: &str) -> Option<ResponseMapping> {
        self.mappings.lock().await.get(response_id).cloned()
    }

    pub async fn put(&self, mapping: ResponseMapping) -> Result<(), ResponseStoreError> {
        let mut line = serde_json::to_vec(&mapping)?;
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(&line).await?;
        writer.flush().await?;
        self.mappings
            .lock()
            .await
            .insert(mapping.response_id.clone(), mapping);
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn parse_response_id(response_id: &str) -> u64 {
    response_id
        .strip_prefix("resp_")
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restores_mappings_and_continues_response_ids() {
        let root = std::env::temp_dir().join(format!(
            "codex-hoshikage-response-store-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root).await;
        let store = ResponseStore::open(&root).await.unwrap();
        store
            .put(ResponseMapping {
                response_id: "resp_7".into(),
                thread_id: "thread_1".into(),
                model_id: "hoshikage/model".into(),
            })
            .await
            .unwrap();
        drop(store);

        let restored = ResponseStore::open(&root).await.unwrap();
        assert_eq!(restored.get("resp_7").await.unwrap().thread_id, "thread_1");
        assert_eq!(restored.next_response_id(), "resp_8");
    }
}
