use anyhow::Result;
use codex_core::config::ConfigBuilder;
use codex_core::config::types::OtelExporterKind;
use codex_core::config::types::OtelHttpProtocol;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use tempfile::TempDir;

const SERVICE_VERSION: &str = "0.0.0-test";

fn set_metrics_exporter(config: &mut codex_core::config::Config) {
    config.otel.metrics_exporter = OtelExporterKind::OtlpHttp {
        endpoint: "http://localhost:4318".to_string(),
        headers: HashMap::new(),
        protocol: OtelHttpProtocol::Json,
        tls: None,
    };
}

#[tokio::test]
async fn app_server_analytics_disabled_by_default() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    set_metrics_exporter(&mut config);
    config.analytics_enabled = None;

    let provider =
        codex_core::otel_init::build_provider(&config, SERVICE_VERSION, Some("codex_app_server"))
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    // With analytics unset, metrics default to disabled. No provider is built.
    assert_eq!(provider.is_none(), true);
    Ok(())
}

#[tokio::test]
async fn app_server_analytics_enabled_when_opted_in() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    set_metrics_exporter(&mut config);
    config.analytics_enabled = Some(true);

    let provider =
        codex_core::otel_init::build_provider(&config, SERVICE_VERSION, Some("codex_app_server"))
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    // With analytics explicitly enabled, metrics are active.
    let has_metrics = provider.as_ref().and_then(|otel| otel.metrics()).is_some();
    assert_eq!(has_metrics, true);
    Ok(())
}
