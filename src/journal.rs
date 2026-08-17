use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("failed to prepare event journal: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to encode event journal entry: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, Serialize)]
pub struct JournalEntry<'a> {
    pub timestamp_ms: u128,
    pub event: &'a str,
    pub response_id: &'a str,
    pub model: &'a str,
    pub status: &'a str,
}

pub struct EventJournal {
    path: PathBuf,
    writer: Mutex<tokio::fs::File>,
}

impl EventJournal {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, JournalError> {
        let directory = root.as_ref().join("state/events");
        fs::create_dir_all(&directory).await?;
        let path = directory.join("events.jsonl");
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            writer: Mutex::new(writer),
        })
    }

    pub async fn append(&self, entry: &JournalEntry<'_>) -> Result<(), JournalError> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(&line).await?;
        writer.flush().await?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn appends_metadata_only_jsonl() {
        let root =
            std::env::temp_dir().join(format!("codex-hoshikage-journal-{}", std::process::id()));
        let journal = EventJournal::open(&root).await.unwrap();
        journal
            .append(&JournalEntry {
                timestamp_ms: 1,
                event: "response.completed",
                response_id: "resp_1",
                model: "model",
                status: "completed",
            })
            .await
            .unwrap();
        let contents = fs::read_to_string(journal.path()).await.unwrap();
        assert!(contents.contains("response.completed"));
        assert!(!contents.contains("prompt"));
    }
}
