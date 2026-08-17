use crate::{
    config::RawModelRegistryConfig,
    model::{ModelError, ModelRegistry, PublicModel, ResolvedModel},
    runtime::CodexRuntime,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};

const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub provider_id: String,
    pub upstream_id: String,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Debug, Default)]
pub struct HttpCatalogSnapshot {
    pub models: Vec<DiscoveredModel>,
    pub available: HashMap<String, bool>,
}

pub struct ModelCatalogManager {
    config: RawModelRegistryConfig,
    runtime: Arc<CodexRuntime>,
    registry: RwLock<ModelRegistry>,
    available: RwLock<HashMap<String, bool>>,
    refresh_lock: Mutex<()>,
}

impl ModelCatalogManager {
    pub fn new(
        config: RawModelRegistryConfig,
        runtime: Arc<CodexRuntime>,
    ) -> Result<Self, ModelError> {
        let registry = ModelRegistry::from_config(&config)?;
        let available = config
            .providers
            .iter()
            .map(|(id, provider)| (id.clone(), provider.enabled && provider.base_url.is_none()))
            .collect();
        Ok(Self {
            config,
            runtime,
            registry: RwLock::new(registry),
            available: RwLock::new(available),
            refresh_lock: Mutex::new(()),
        })
    }

    pub async fn refresh(&self) {
        let _guard = self.refresh_lock.lock().await;
        let http = discover_http_models(&self.config).await;
        let mut registry = match ModelRegistry::from_config(&self.config) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::error!(error = %error, "model registry could not be rebuilt");
                return;
            }
        };
        for model in &http.models {
            if let Err(error) = registry.add_discovered_model(
                model.provider_id.clone(),
                model.upstream_id.clone(),
                model.reasoning_efforts.clone(),
            ) {
                tracing::warn!(error = %error, "discovered model could not be registered");
            }
        }

        let mut available = self
            .config
            .providers
            .iter()
            .map(|(id, provider)| (id.clone(), provider.enabled && provider.base_url.is_none()))
            .collect::<HashMap<_, _>>();
        available.extend(http.available);
        let non_chatgpt_upstream_ids = self
            .config
            .models
            .values()
            .filter(|model| model.provider_id != "chatgpt")
            .map(|model| model.upstream_id.clone())
            .chain(
                http.models
                    .iter()
                    .filter(|model| model.provider_id != "chatgpt")
                    .map(|model| model.upstream_id.clone()),
            )
            .collect::<HashSet<_>>();

        if self
            .config
            .providers
            .get("chatgpt")
            .is_some_and(|provider| provider.enabled)
        {
            match tokio::time::timeout(
                MODEL_CATALOG_TIMEOUT,
                self.runtime
                    .request("model/list", json!({"limit": 1000, "includeHidden": false})),
            )
            .await
            {
                Ok(Ok(result)) => {
                    let mut imported = 0;
                    if let Some(data) = result.get("data").and_then(|value| value.as_array()) {
                        for model in data {
                            let Some(upstream_id) =
                                model.get("id").and_then(|value| value.as_str())
                            else {
                                continue;
                            };
                            let provider = model
                                .get("modelProvider")
                                .or_else(|| model.get("model_provider"))
                                .and_then(|value| value.as_str());
                            if non_chatgpt_upstream_ids.contains(upstream_id)
                                || provider
                                    .is_some_and(|value| value != "openai" && value != "chatgpt")
                            {
                                continue;
                            }
                            let efforts = model
                                .get("supportedReasoningEfforts")
                                .and_then(|value| value.as_array())
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(|value| value.get("reasoningEffort"))
                                        .filter_map(|value| value.as_str())
                                        .map(str::to_owned)
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            if registry
                                .add_discovered_model("chatgpt".into(), upstream_id.into(), efforts)
                                .is_ok()
                            {
                                imported += 1;
                            }
                        }
                    }
                    available.insert("chatgpt".into(), true);
                    tracing::info!(
                        chatgpt_imported = imported,
                        "refreshed ChatGPT model catalog"
                    );
                }
                Ok(Err(error)) => {
                    available.insert("chatgpt".into(), false);
                    tracing::warn!(error = %error, "ChatGPT model catalog unavailable");
                }
                Err(_) => {
                    available.insert("chatgpt".into(), false);
                    tracing::warn!("ChatGPT model catalog timed out");
                }
            }
        }
        *self.registry.write().await = registry;
        *self.available.write().await = available;
    }

    pub fn provider_limits(&self) -> HashMap<String, usize> {
        self.config
            .providers
            .iter()
            .filter(|(_, provider)| provider.enabled)
            .map(|(id, provider)| (id.clone(), provider.max_concurrent_turns))
            .collect()
    }

    pub async fn list_public_models(&self) -> Vec<PublicModel> {
        self.refresh().await;
        let registry = self.registry.read().await;
        let available = self.available.read().await;
        registry
            .list_public_models()
            .into_iter()
            .filter(|model| available.get(&model.owned_by).copied().unwrap_or(false))
            .collect()
    }

    pub async fn resolve(
        &self,
        requested_model: Option<&str>,
        requested_reasoning: Option<&str>,
    ) -> Result<ResolvedModel, ModelError> {
        self.refresh().await;
        let public_model_id = requested_model
            .filter(|value| !value.is_empty() && *value != "default")
            .unwrap_or(&self.config.default_model);
        if let Some((provider_id, _)) = public_model_id.split_once('/')
            && self.available.read().await.get(provider_id) == Some(&false)
        {
            return Err(ModelError::ProviderUnavailable(provider_id.into()));
        }
        let registry = self.registry.read().await;
        let resolved = registry.resolve(requested_model, requested_reasoning)?;
        if !self
            .available
            .read()
            .await
            .get(&resolved.public_provider_id)
            .copied()
            .unwrap_or(false)
        {
            return Err(ModelError::ProviderUnavailable(resolved.public_provider_id));
        }
        Ok(resolved)
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModel {
    id: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HoshikageModelsResponse {
    #[serde(default)]
    data: Vec<HoshikageModel>,
}

#[derive(Debug, Deserialize)]
struct HoshikageModel {
    id: String,
    #[serde(default)]
    tools: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaModelsResponse {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    model: String,
}

pub async fn discover_http_models(config: &RawModelRegistryConfig) -> HttpCatalogSnapshot {
    let client = match reqwest::Client::builder()
        .timeout(MODEL_CATALOG_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(error = %error, "model catalog client could not be created");
            return HttpCatalogSnapshot::default();
        }
    };
    let mut snapshot = HttpCatalogSnapshot::default();
    for (provider_id, provider) in &config.providers {
        if !provider.enabled {
            continue;
        }
        let Some(base_url) = provider.base_url.as_deref() else {
            continue;
        };
        let endpoint = match provider_id.as_str() {
            "ollama" => format!(
                "{}/api/tags",
                base_url.trim_end_matches('/').trim_end_matches("/v1")
            ),
            _ => format!("{}/models", base_url.trim_end_matches('/')),
        };
        let mut request = client.get(endpoint);
        if let Some(env_key) = provider.auth_env_key.as_deref()
            && let Ok(token) = std::env::var(env_key)
        {
            request = request.bearer_auth(token);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(provider = %provider_id, error = %error, "model catalog unavailable");
                snapshot.available.insert(provider_id.clone(), false);
                continue;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(provider = %provider_id, status = %response.status(), "model catalog returned an error");
            snapshot.available.insert(provider_id.clone(), false);
            continue;
        }
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(provider = %provider_id, error = %error, "model catalog response could not be read");
                snapshot.available.insert(provider_id.clone(), false);
                continue;
            }
        };
        if provider_id == "ollama" {
            match serde_json::from_slice::<OllamaModelsResponse>(&body) {
                Ok(models) => {
                    snapshot.available.insert(provider_id.clone(), true);
                    snapshot
                        .models
                        .extend(models.models.into_iter().map(|model| {
                            let upstream_id = if model.model.is_empty() {
                                model.name.clone()
                            } else {
                                model.model
                            };
                            DiscoveredModel {
                                provider_id: provider_id.clone(),
                                upstream_id,
                                reasoning_efforts: Vec::new(),
                            }
                        }))
                }
                Err(error) => {
                    tracing::warn!(provider = %provider_id, error = %error, "Ollama model catalog could not be decoded")
                }
            }
        } else {
            let tool_capable_ids = if provider_id == "hoshikage" {
                match fetch_hoshikage_tool_capabilities(&client, base_url, provider).await {
                    Some(ids) => Some(ids),
                    None => continue,
                }
            } else {
                None
            };
            match serde_json::from_slice::<OpenAiModelsResponse>(&body) {
                Ok(models) => {
                    snapshot.available.insert(provider_id.clone(), true);
                    snapshot.models.extend(
                        models
                            .data
                            .into_iter()
                            .filter(|model| {
                                tool_capable_ids
                                    .as_ref()
                                    .is_none_or(|ids| ids.contains(&model.id))
                            })
                            .map(|model| DiscoveredModel {
                                provider_id: provider_id.clone(),
                                upstream_id: model.id,
                                reasoning_efforts: model.supported_reasoning_levels,
                            }),
                    )
                }
                Err(error) => {
                    tracing::warn!(provider = %provider_id, error = %error, "model catalog could not be decoded")
                }
            }
        }
    }
    snapshot
}

async fn fetch_hoshikage_tool_capabilities(
    client: &reqwest::Client,
    base_url: &str,
    provider: &crate::config::RawProviderConfig,
) -> Option<std::collections::HashSet<String>> {
    let endpoint = format!(
        "{}/v1/hoshikage/models",
        base_url.trim_end_matches('/').trim_end_matches("/v1")
    );
    let mut request = client.get(endpoint);
    if let Some(env_key) = provider.auth_env_key.as_deref()
        && let Ok(token) = std::env::var(env_key)
    {
        request = request.bearer_auth(token);
    }
    let response = match request.send().await {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            tracing::warn!(status = %response.status(), "Hoshikage detailed model catalog returned an error");
            return None;
        }
        Err(error) => {
            tracing::warn!(error = %error, "Hoshikage detailed model catalog unavailable");
            return None;
        }
    };
    let catalog = match response.json::<HoshikageModelsResponse>().await {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(error = %error, "Hoshikage detailed model catalog could not be decoded");
            return None;
        }
    };
    Some(
        catalog
            .data
            .into_iter()
            .filter(|model| model.tools)
            .map(|model| model.id)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hoshikage_detail_catalog_keeps_only_tool_capable_models() {
        let body = r#"{
            "data": [
                {"id":"gemma-tool","tools":true},
                {"id":"lfm-text-only","tools":false}
            ]
        }"#;
        let catalog: HoshikageModelsResponse = serde_json::from_str(body).unwrap();
        let ids: std::collections::HashSet<_> = catalog
            .data
            .into_iter()
            .filter(|model| model.tools)
            .map(|model| model.id)
            .collect();
        assert!(ids.contains("gemma-tool"));
        assert!(!ids.contains("lfm-text-only"));
    }
}
