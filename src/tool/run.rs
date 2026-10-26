//! Running is one submission and one directory of artifacts: the tool answers with the
//! directory, because the caller knows what it sent and the artifacts carry the rest.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Tool};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value as Json};

use crate::config::{self, Descriptions};
use crate::core::comfy::Client;
use crate::core::execution;

use super::{Server, args_schema, fail, text, unknown};

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct RunArgs {
    name: String,
    #[serde(default)]
    params: Option<Map<String, Json>>,
    #[serde(default)]
    output_path: Option<String>,
}

#[tool_router(router = run_router, vis = "pub(super)")]
impl Server {
    #[tool]
    async fn workflow_run(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let loaded = match config::load() {
            Ok(loaded) => loaded,
            Err(err) => return Ok(fail(err)),
        };

        let Some(workflow) = loaded.workflows.get(&args.name) else {
            return Ok(fail(unknown(&loaded.workflows, &args.name)));
        };

        // The TLS backend follows the URL scheme, so the client is rebuilt with the
        // configuration: a run takes tens of seconds, which makes this negligible.
        let client = match Client::new(&loaded.env.base_url) {
            Ok(client) => client,
            Err(err) => return Ok(fail(err.to_string())),
        };

        let root = match output_root(
            args.output_path.as_deref(),
            &loaded.setting.paths.output_dir,
            &crate::core::home::dir(),
        ) {
            Ok(root) => root,
            Err(err) => return Ok(fail(err)),
        };

        let timeout = Duration::from_secs(loaded.setting.timings.timeout_seconds);
        let poll_interval = Duration::from_millis(loaded.setting.timings.poll_interval_ms);

        tracing::info!(workflow = %workflow.name, "workflow_run");
        match execution::execute(
            &client,
            workflow,
            args.params.as_ref(),
            &root,
            timeout,
            poll_interval,
        )
        .await
        {
            Ok(dir) => Ok(text(dir.display().to_string())),
            Err(err) => Ok(fail(err.to_string())),
        }
    }
}

pub(super) fn tool(descriptions: &Descriptions) -> Tool {
    Tool::new(
        "workflow_run",
        descriptions.run_tool.clone(),
        args_schema::<RunArgs>(&[
            ("name", &descriptions.name_arg),
            ("output_path", &descriptions.output_path_arg),
        ]),
    )
}

/// Only an absolute path or `~/` is accepted: the server's cwd is chosen by whoever
/// spawned it, so a relative path would land somewhere different for every agent.
fn output_root(raw: Option<&str>, default_root: &Path, home: &Path) -> Result<PathBuf, String> {
    match raw {
        None => Ok(default_root.to_path_buf()),
        Some(raw) => expand_output_root(raw, home),
    }
}

fn expand_output_root(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("output_path is empty".to_string());
    }

    let path = if raw == "~" || raw.starts_with("~/") {
        if raw == "~" {
            home.to_path_buf()
        } else {
            home.join(&raw[2..])
        }
    } else {
        PathBuf::from(raw)
    };

    if !path.is_absolute() {
        return Err(format!(
            "output_path must be absolute or start with ~: {raw}"
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{expand_output_root, output_root};
    use std::path::{Path, PathBuf};

    #[test]
    fn absolute_output_path_is_kept() {
        assert_eq!(
            expand_output_root("/data/out", Path::new("/home/u")).unwrap(),
            PathBuf::from("/data/out")
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(
            expand_output_root("~/images", Path::new("/home/u")).unwrap(),
            PathBuf::from("/home/u/images")
        );
        assert_eq!(
            expand_output_root("~", Path::new("/home/u")).unwrap(),
            PathBuf::from("/home/u")
        );
        assert!(expand_output_root("~x", Path::new("/home/u")).is_err());
    }

    #[test]
    fn relative_and_empty_are_rejected() {
        assert!(expand_output_root("out", Path::new("/home/u")).is_err());
        assert!(expand_output_root("", Path::new("/home/u")).is_err());
        assert!(expand_output_root("   ", Path::new("/home/u")).is_err());
    }

    #[test]
    fn omitting_the_override_falls_back_to_the_configured_root() {
        assert_eq!(
            output_root(None, Path::new("/cfg/out"), Path::new("/home/u")).unwrap(),
            PathBuf::from("/cfg/out")
        );
        assert_eq!(
            output_root(Some("/other"), Path::new("/cfg/out"), Path::new("/home/u")).unwrap(),
            PathBuf::from("/other")
        );
    }
}
