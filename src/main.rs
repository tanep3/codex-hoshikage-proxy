use codex_hoshikage_proxy::{
    config::{ValidatedConfig, default_config_path},
    http::{AppState, router},
    model::ModelRegistry,
    runtime::CodexRuntime,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let config_path = default_config_path();
    let config = ValidatedConfig::load(&config_path)?;
    config.prepare_codex_home()?;
    let models = ModelRegistry::from_config(&config.models)?;
    let runtime = CodexRuntime::launch(&config).await?;
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    tracing::info!(address = %config.listen_addr, "Codex Hoshikage Proxy listening");
    axum::serve(
        listener,
        router(AppState::new(
            runtime.clone(),
            models,
            config.cwd_policy.clone(),
            config.default_cwd.clone(),
        )),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = runtime.shutdown().await;
    })
    .await?;
    Ok(())
}
