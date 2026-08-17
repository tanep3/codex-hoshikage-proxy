use crate::config::RawModelRegistryConfig;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub public_model_id: String,
    pub public_provider_id: String,
    pub codex_provider_id: String,
    pub upstream_model_id: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("model not found: {0}")]
    NotFound(String),
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    #[error("invalid model registry: {0}")]
    InvalidRegistry(String),
    #[error("unsupported parameter: reasoning effort is not supported by provider {0}")]
    UnsupportedReasoning(String),
    #[error("unsupported reasoning effort: {0}")]
    UnsupportedEffort(String),
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    default_model: String,
    providers: HashMap<String, ProviderDefinition>,
    models: HashMap<String, ModelDefinition>,
}

#[derive(Debug, Clone)]
struct ProviderDefinition {
    codex_id: String,
    enabled: bool,
    max_concurrent_turns: usize,
}

#[derive(Debug, Clone)]
struct ModelDefinition {
    provider_id: String,
    upstream_id: String,
    supported_reasoning_efforts: Vec<ReasoningEffort>,
    default_reasoning_effort: Option<ReasoningEffort>,
}

impl ModelRegistry {
    pub fn from_config(config: &RawModelRegistryConfig) -> Result<Self, ModelError> {
        if config.default_model.trim().is_empty() {
            return Err(ModelError::InvalidRegistry("default_model is empty".into()));
        }
        let providers = config
            .providers
            .iter()
            .map(|(id, provider)| {
                if provider.codex_id.trim().is_empty() || provider.max_concurrent_turns == 0 {
                    return Err(ModelError::InvalidRegistry(format!(
                        "invalid provider: {id}"
                    )));
                }
                Ok((
                    id.clone(),
                    ProviderDefinition {
                        codex_id: provider.codex_id.clone(),
                        enabled: provider.enabled,
                        max_concurrent_turns: provider.max_concurrent_turns,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, ModelError>>()?;
        let models = config
            .models
            .iter()
            .map(|(id, model)| {
                let supported = model
                    .reasoning_efforts
                    .iter()
                    .map(|value| {
                        ReasoningEffort::parse(value).ok_or_else(|| {
                            ModelError::InvalidRegistry(format!(
                                "unknown reasoning effort: {value}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let default = model
                    .default_reasoning_effort
                    .as_deref()
                    .map(|value| {
                        ReasoningEffort::parse(value).ok_or_else(|| {
                            ModelError::InvalidRegistry(format!(
                                "unknown reasoning effort: {value}"
                            ))
                        })
                    })
                    .transpose()?;
                if default.is_some_and(|value| !supported.contains(&value)) {
                    return Err(ModelError::InvalidRegistry(format!(
                        "default reasoning effort is not supported by model: {id}"
                    )));
                }
                if !providers.contains_key(&model.provider_id) {
                    return Err(ModelError::InvalidRegistry(format!(
                        "model references unknown provider: {id}"
                    )));
                }
                Ok((
                    id.clone(),
                    ModelDefinition {
                        provider_id: model.provider_id.clone(),
                        upstream_id: model.upstream_id.clone(),
                        supported_reasoning_efforts: supported,
                        default_reasoning_effort: default,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, ModelError>>()?;
        if !models.contains_key(&config.default_model) {
            return Err(ModelError::InvalidRegistry(
                "default_model is not registered".into(),
            ));
        }
        Ok(Self {
            default_model: config.default_model.clone(),
            providers,
            models,
        })
    }

    pub fn resolve(
        &self,
        requested_model: Option<&str>,
        requested_reasoning: Option<&str>,
    ) -> Result<ResolvedModel, ModelError> {
        let public_model_id =
            match requested_model.filter(|value| !value.is_empty() && *value != "default") {
                Some(value) => value,
                None => &self.default_model,
            };
        let model = self
            .models
            .get(public_model_id)
            .ok_or_else(|| ModelError::NotFound(public_model_id.to_string()))?;
        let provider = self
            .providers
            .get(&model.provider_id)
            .ok_or_else(|| ModelError::InvalidRegistry("provider disappeared".into()))?;
        if !provider.enabled {
            return Err(ModelError::ProviderUnavailable(model.provider_id.clone()));
        }
        let reasoning_effort = match requested_reasoning {
            Some(_) if model.provider_id != "chatgpt" => {
                return Err(ModelError::UnsupportedReasoning(model.provider_id.clone()));
            }
            Some(value) => {
                let effort = ReasoningEffort::parse(value)
                    .ok_or_else(|| ModelError::UnsupportedEffort(value.into()))?;
                if !model.supported_reasoning_efforts.contains(&effort) {
                    return Err(ModelError::UnsupportedEffort(value.into()));
                }
                Some(effort)
            }
            None => model.default_reasoning_effort,
        };
        let _ = provider.max_concurrent_turns;
        Ok(ResolvedModel {
            public_model_id: public_model_id.to_string(),
            public_provider_id: model.provider_id.clone(),
            codex_provider_id: provider.codex_id.clone(),
            upstream_model_id: model.upstream_id.clone(),
            reasoning_effort,
            supported_reasoning_efforts: model.supported_reasoning_efforts.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RawModelRegistryConfig;

    #[test]
    fn resolves_default_hoshikage_model() {
        let registry = ModelRegistry::from_config(&RawModelRegistryConfig::default()).unwrap();
        let model = registry.resolve(None, None).unwrap();
        assert_eq!(model.public_provider_id, "hoshikage");
        assert_eq!(
            model.upstream_model_id,
            "unsloth-gemma4-12b-qat-thinking-off"
        );
    }

    #[test]
    fn rejects_reasoning_effort_for_non_chatgpt_provider() {
        let registry = ModelRegistry::from_config(&RawModelRegistryConfig::default()).unwrap();
        assert_eq!(
            registry.resolve(None, Some("medium")),
            Err(ModelError::UnsupportedReasoning("hoshikage".into()))
        );
    }

    #[test]
    fn accepts_reasoning_effort_for_chatgpt_public_provider() {
        let mut config = RawModelRegistryConfig::default();
        config.providers.insert(
            "chatgpt".into(),
            crate::config::RawProviderConfig {
                codex_id: "openai".into(),
                enabled: true,
                max_concurrent_turns: 4,
                base_url: None,
                auth_env_key: None,
            },
        );
        config.models.insert(
            "chatgpt/gpt-5.6-sol".into(),
            crate::config::RawModelConfig {
                provider_id: "chatgpt".into(),
                upstream_id: "gpt-5.6-sol".into(),
                display_name: "GPT-5.6 Sol".into(),
                reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
                default_reasoning_effort: Some("medium".into()),
            },
        );
        let registry = ModelRegistry::from_config(&config).unwrap();
        let model = registry
            .resolve(Some("chatgpt/gpt-5.6-sol"), Some("high"))
            .unwrap();
        assert_eq!(model.codex_provider_id, "openai");
        assert_eq!(model.reasoning_effort, Some(ReasoningEffort::High));
    }
}
