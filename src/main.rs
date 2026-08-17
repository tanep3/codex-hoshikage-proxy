use codex_hoshikage_proxy::{
    catalog::ModelCatalogManager,
    config::{ValidatedConfig, default_config_path},
    http::{AppState, router},
    journal::EventJournal,
    runtime::CodexRuntime,
    store::ResponseStore,
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
    let responses = std::sync::Arc::new(
        ResponseStore::open(
            config
                .codex_home
                .parent()
                .unwrap_or(config.codex_home.as_path()),
        )
        .await?,
    );
    let runtime = CodexRuntime::launch(&config).await?;
    let catalog = ModelCatalogManager::new(config.models.clone(), runtime.clone())?;
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    tracing::info!(address = %config.listen_addr, "Codex Hoshikage Proxy listening");
    axum::serve(
        listener,
        router(AppState::new(
            runtime.clone(),
            catalog,
            config.cwd_policy.clone(),
            config.default_cwd.clone(),
            config.api_key.clone(),
            std::time::Duration::from_secs(config.approval_timeout_seconds),
            config.auto_approve_workspace,
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
