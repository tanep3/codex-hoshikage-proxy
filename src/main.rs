use codex_hoshikage_proxy::{
    config::{ValidatedConfig, default_config_path},
    http::{AppState, router},
    journal::EventJournal,
    model::ModelRegistry,
    runtime::CodexRuntime,
};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

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
            journal,
        )),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = runtime.shutdown().await;
    })
    .await?;
    Ok(())
}
