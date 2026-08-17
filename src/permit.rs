use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Error)]
pub enum PermitError {
    #[error("provider permit pool does not contain provider: {0}")]
    UnknownProvider(String),
    #[error("provider permit pool was closed for provider: {0}")]
    Closed(String),
}

#[derive(Clone)]
pub struct ProviderPermitPool {
    semaphores: Arc<HashMap<String, Arc<Semaphore>>>,
}

impl ProviderPermitPool {
    pub fn new(limits: HashMap<String, usize>) -> Self {
        let semaphores = limits
            .into_iter()
            .map(|(provider, limit)| (provider, Arc::new(Semaphore::new(limit))))
            .collect();
        Self {
            semaphores: Arc::new(semaphores),
        }
    }

    pub async fn acquire(&self, provider: &str) -> Result<OwnedSemaphorePermit, PermitError> {
        let semaphore = self
            .semaphores
            .get(provider)
            .ok_or_else(|| PermitError::UnknownProvider(provider.into()))?
            .clone();
        semaphore
            .acquire_owned()
            .await
            .map_err(|_| PermitError::Closed(provider.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limits_concurrent_acquisition() {
        let pool = ProviderPermitPool::new(HashMap::from([(String::from("ollama"), 1)]));
        let first = pool.acquire("ollama").await.unwrap();
        let pending =
            tokio::time::timeout(std::time::Duration::from_millis(20), pool.acquire("ollama"))
                .await;
        assert!(pending.is_err());
        drop(first);
        assert!(pool.acquire("ollama").await.is_ok());
    }
}
