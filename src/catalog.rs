use crate::config::RawModelRegistryConfig;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub provider_id: String,
    pub upstream_id: String,
    pub reasoning_efforts: Vec<String>,
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

pub async fn discover_http_models(config: &RawModelRegistryConfig) -> Vec<DiscoveredModel> {
    let client = reqwest::Client::new();
    let mut discovered = Vec::new();
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
        if let Some(env_key) = provider.auth_env_key.as_deref() {
            if let Ok(token) = std::env::var(env_key) {
                request = request.bearer_auth(token);
            }
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(provider = %provider_id, error = %error, "model catalog unavailable");
                continue;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(provider = %provider_id, status = %response.status(), "model catalog returned an error");
            continue;
        }
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(provider = %provider_id, error = %error, "model catalog response could not be read");
                continue;
            }
        };
        if provider_id == "ollama" {
            match serde_json::from_slice::<OllamaModelsResponse>(&body) {
                Ok(models) => discovered.extend(models.models.into_iter().map(|model| {
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
                })),
                Err(error) => {
                    tracing::warn!(provider = %provider_id, error = %error, "Ollama model catalog could not be decoded")
                }
            }
        } else {
            match serde_json::from_slice::<OpenAiModelsResponse>(&body) {
                Ok(models) => {
                    discovered.extend(models.data.into_iter().map(|model| DiscoveredModel {
                        provider_id: provider_id.clone(),
                        upstream_id: model.id,
                        reasoning_efforts: model.supported_reasoning_levels,
                    }))
                }
                Err(error) => {
                    tracing::warn!(provider = %provider_id, error = %error, "model catalog could not be decoded")
                }
            }
        }
    }
    discovered
}
