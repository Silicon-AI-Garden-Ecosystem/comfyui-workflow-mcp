//! Startup only. The tool surface re-reads the configuration and rescans the workflows
//! on every call, so a workflow added minutes ago works without a restart.

use std::path::Path;
use std::process::ExitCode;

use tracing::{Level, debug, enabled, error, info, warn};

use crate::config;
use crate::core::{comfy, logging};

pub async fn run() -> ExitCode {
    let config = match config::load() {
        Ok(config) => config,
        Err(err) => {
            // The logger needs `logs_dir`, so a configuration failure can only go to stderr.
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let setting = &config.setting;
    logging::init(
        &setting.paths.logs_dir,
        logging::parse_level(setting.log_level.as_str()),
    );

    // The configuration path is fixed, so the log names it once instead of tagging every value.
    info!("config           $XDG_CONFIG_HOME/comfyui-workflow-mcp/config.toml");
    info!("log_level        {}", setting.log_level.as_str());
    info!("env_file         {}", setting.paths.env_file.display());
    info!("logs_dir         {}", setting.paths.logs_dir.display());
    info!("output_dir       {}", setting.paths.output_dir.display());
    info!("workflows_dir    {}", setting.paths.workflows_dir.display());
    info!("timeout_seconds  {}", setting.timings.timeout_seconds);
    info!("poll_interval_ms {}", setting.timings.poll_interval_ms);

    // No connection at startup: every spawn would pay a round trip. The debug probe
    // tells a wrong address apart from wrong workflow parameters.
    if enabled!(Level::DEBUG) {
        match comfy::Client::new(&config.env.base_url) {
            Ok(client) => match client.system_stats().await {
                Ok(version) => debug!("base_url         reachable, comfyui {version}"),
                Err(err) => debug!("base_url         unreachable: {err}"),
            },
            Err(err) => debug!("base_url         client init failed: {err}"),
        }
    }

    if let Err(err) = prepare_output_dir(&setting.paths.output_dir) {
        error!(
            "output_dir {} is not writable: {err}",
            setting.paths.output_dir.display()
        );
        return ExitCode::FAILURE;
    }
    // A broken workflow is left out of the set and the others keep working: one
    // author's typo must not take down the server. The reason goes to the log.
    for err in config.workflows.broken() {
        warn!("workflow skipped: {err}");
    }
    for workflow in config.workflows.all() {
        info!(
            "  {}  ok  {} params ({} required)",
            workflow.name,
            workflow.manifest.params.len(),
            workflow
                .manifest
                .params
                .values()
                .filter(|p| p.required())
                .count()
        );
    }
    info!("{} workflows ready", config.workflows.len());

    crate::tool::serve().await
}

/// Probed once at startup: a read-only mount must surface here, not after a 30 second run.
fn prepare_output_dir(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let probe = root.join(".write-probe");
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())?;
    Ok(())
}
