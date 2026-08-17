use codex_hoshikage_proxy::{
    catalog::discover_http_models,
    config::{ValidatedConfig, default_config_path},
    http::{AppState, router},
    journal::EventJournal,
    model::ModelRegistry,
    runtime::CodexRuntime,
    store::ResponseStore,
};
use serde_json::json;
use std::collections::HashSet;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

fn model_id_variants(model_id: &str) -> impl Iterator<Item = &str> {
    std::iter::once(model_id).chain(model_id.rsplit_once('/').map(|(_, id)| id))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into()))
        .init();
    let config_path = default_config_path();
    let config = ValidatedConfig::load(&config_path)?;
    config.prepare_codex_home()?;
    let journal = std::sync::Arc::new(
        EventJournal::open(
            config
                .codex_home
                .parent()
                .unwrap_or(config.codex_home.as_path()),
        )
        .await?,
    );
    let responses = std::sync::Arc::new(
        ResponseStore::open(
            config
                .codex_home
                .parent()
                .unwrap_or(config.codex_home.as_path()),
        )
        .await?,
    );
    let discovered = discover_http_models(&config.models).await;
    let mut non_chatgpt_upstream_ids = config
        .models
        .models
        .values()
        .filter(|model| model.provider_id != "chatgpt")
        .flat_map(|model| model_id_variants(&model.upstream_id).map(str::to_owned))
        .collect::<HashSet<_>>();
    non_chatgpt_upstream_ids.extend(
        discovered
            .iter()
            .filter(|model| model.provider_id != "chatgpt")
            .flat_map(|model| model_id_variants(&model.upstream_id).map(str::to_owned)),
    );
    tracing::info!(
        external_model_count = non_chatgpt_upstream_ids.len(),
        "prepared provider-aware model catalog"
    );
    let mut models = ModelRegistry::from_config(&config.models)?;
    for model in discovered {
        models.add_discovered_model(
            model.provider_id,
            model.upstream_id,
            model.reasoning_efforts,
        )?;
    }
    let runtime = CodexRuntime::launch(&config).await?;
    if config
        .models
        .providers
        .get("chatgpt")
        .is_some_and(|provider| provider.enabled)
    {
        if !config.codex_home.join("auth.json").is_file() {
            tracing::warn!(
                path = %config.codex_home.join("auth.json").display(),
                "ChatGPT provider is enabled but dedicated Codex auth is missing; run CODEX_HOME=<proxy codex-home> codex login --device-auth"
            );
        }
        match runtime
            .request("model/list", json!({"limit": 1000, "includeHidden": false}))
            .await
        {
            Ok(result) => {
                if let Some(data) = result.get("data").and_then(|value| value.as_array()) {
                    let mut imported = 0usize;
                    let mut excluded = 0usize;
                    for model in data {
                        let Some(upstream_id) = model.get("id").and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let provider_from_catalog = model
                            .get("modelProvider")
                            .or_else(|| model.get("model_provider"))
                            .and_then(|value| value.as_str());
                        if model_id_variants(upstream_id)
                            .any(|id| non_chatgpt_upstream_ids.contains(id))
                            || provider_from_catalog.is_some_and(|provider| {
                                provider != "openai" && provider != "chatgpt"
                            })
                        {
                            excluded += 1;
                            continue;
                        }
                        let reasoning_efforts = model
                            .get("supportedReasoningEfforts")
                            .and_then(|value| value.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| {
                                        value
                                            .get("reasoningEffort")
                                            .and_then(|effort| effort.as_str())
                                    })
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        models.add_discovered_model(
                            "chatgpt".into(),
                            upstream_id.into(),
                            reasoning_efforts,
                        )?;
                        imported += 1;
                    }
                    tracing::info!(
                        codex_catalog_count = data.len(),
                        chatgpt_imported = imported,
                        external_excluded = excluded,
                        "processed Codex model catalog"
                    );
                }
            }
            Err(error) => tracing::warn!(error = %error, "ChatGPT model catalog unavailable"),
        }
    }
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    tracing::info!(address = %config.listen_addr, "Codex Hoshikage Proxy listening");
    axum::serve(
        listener,
        router(AppState::new(
            runtime.clone(),
            models,
            config.cwd_policy.clone(),
            config.default_cwd.clone(),
            config.api_key.clone(),
            std::time::Duration::from_secs(config.approval_timeout_seconds),
            journal,
            responses,
        )),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = runtime.shutdown().await;
    })
    .await?;
    Ok(())
}
