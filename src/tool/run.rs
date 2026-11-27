//! Running is one submission and one directory of artifacts:
//! the tool answers with the directory,
//! because the caller knows what it sent and the artifacts carry the rest.

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
use crate::core::messages::{Messages, fill};

use super::{Server, args_schema, fail, text, unknown_host, unknown_workflow};

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct RunArgs {
    #[serde(default)]
    host_name: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
    #[serde(default)]
    params: Option<Map<String, Json>>,
    #[serde(default)]
    output_path: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[tool_router(router = run_router, vis = "pub(super)")]
impl Server {
    #[tool]
    async fn run(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let loaded = match config::load() {
            Ok(loaded) => loaded,
            Err(err) => return Ok(fail(err)),
        };

        let messages = &loaded.setting.messages;
        let default_path = loaded.setting.paths.default_args_file.display().to_string();
        let defaults = match &loaded.default_args {
            Ok(defaults) => Some(defaults),
            Err(err) if args.host_name.is_some() && args.workflow_name.is_some() => {
                tracing::warn!(error = err.as_str(), "default args ignored");
                None
            }
            Err(err) => return Ok(fail(err.clone())),
        };

        let host_defaulted = args.host_name.is_none();
        let Some(host_name) = args
            .host_name
            .or_else(|| defaults.and_then(|d| d.host_name.clone()))
        else {
            return Ok(fail(fill(
                &messages.tools.missing_arg,
                &[("key", "host_name"), ("path", &default_path)],
            )));
        };

        let workflow_defaulted = args.workflow_name.is_none();
        let Some(workflow_name) = args
            .workflow_name
            .or_else(|| defaults.and_then(|d| d.workflow_name.clone()))
        else {
            return Ok(fail(fill(
                &messages.tools.missing_arg,
                &[("key", "workflow_name"), ("path", &default_path)],
            )));
        };

        let Some(host) = loaded.workflows.host(&host_name) else {
            let mut message = unknown_host(&loaded.workflows, &host_name, messages);
            if host_defaulted {
                message.push_str(&fill(
                    &messages.tools.from_defaults,
                    &[("path", &default_path)],
                ));
            }
            return Ok(fail(message));
        };
        let Some(workflow) = loaded.workflows.get(&host_name, &workflow_name) else {
            let mut message =
                unknown_workflow(&loaded.workflows, &host_name, &workflow_name, messages);
            if workflow_defaulted {
                message.push_str(&fill(
                    &messages.tools.from_defaults,
                    &[("path", &default_path)],
                ));
            }
            return Ok(fail(message));
        };

        // The TLS backend follows the URL scheme,
        // so the client is rebuilt with the configuration:
        // a run takes tens of seconds, which makes this negligible.
        let client = match Client::new(&host.base_url) {
            Ok(client) => client,
            Err(err) => return Ok(fail(err.render(messages))),
        };

        let output_path = args
            .output_path
            .as_deref()
            .or_else(|| defaults.and_then(|d| d.output_path.as_deref()));
        let root = match output_root(
            output_path,
            &loaded.setting.paths.output_dir,
            &crate::core::home::dir(),
            messages,
        ) {
            Ok(root) => root,
            Err(err) => return Ok(fail(err)),
        };

        let requested_timeout = args
            .timeout_seconds
            .or_else(|| defaults.and_then(|d| d.timeout_seconds));
        let timeout_seconds = match requested_timeout {
            None => loaded.setting.timings.timeout_seconds,
            Some(0) => {
                return Ok(fail(fill(
                    &messages.tools.timeout_must_be_positive,
                    &[("value", "0")],
                )));
            }
            Some(seconds) => seconds,
        };
        let timeout = Duration::from_secs(timeout_seconds);
        let poll_interval = Duration::from_millis(loaded.setting.timings.poll_interval_ms);

        tracing::info!(
            host = %workflow.host,
            workflow = %workflow.name,
            host_defaulted,
            workflow_defaulted,
            "run"
        );
        match execution::execute(
            &client,
            workflow,
            args.params.as_ref(),
            &root,
            timeout,
            poll_interval,
            messages,
        )
        .await
        {
            Ok(dir) => Ok(text(dir.display().to_string())),
            Err(err) => Ok(fail(err.render(messages))),
        }
    }
}

pub(super) fn tool(descriptions: &Descriptions) -> Tool {
    let host_arg = format!(
        "{} {}",
        descriptions.host_name_arg, descriptions.default_args_note
    );
    let workflow_arg = format!(
        "{} {}",
        descriptions.workflow_name_arg, descriptions.default_args_note
    );
    Tool::new(
        "run",
        descriptions.run_tool.clone(),
        args_schema::<RunArgs>(&[
            ("host_name", host_arg.as_str()),
            ("workflow_name", workflow_arg.as_str()),
            ("output_path", &descriptions.output_path_arg),
            ("timeout_seconds", &descriptions.timeout_seconds_arg),
        ]),
    )
}

// Only an absolute path or `~/` is accepted:
// the server's cwd is chosen by whoever spawned it,
// so a relative path would land somewhere different for every agent.
fn output_root(
    raw: Option<&str>,
    default_root: &Path,
    home: &Path,
    messages: &Messages,
) -> Result<PathBuf, String> {
    match raw {
        None => Ok(default_root.to_path_buf()),
        Some(raw) => expand_output_root(raw, home, messages),
    }
}

fn expand_output_root(raw: &str, home: &Path, messages: &Messages) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(messages.tools.output_path_empty.clone());
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
        return Err(fill(
            &messages.tools.output_path_not_absolute,
            &[("raw", raw)],
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{expand_output_root, output_root};
    use crate::core::messages::builtin;
    use std::path::{Path, PathBuf};

    #[test]
    fn absolute_output_path_is_kept() {
        assert_eq!(
            expand_output_root("/data/out", Path::new("/home/u"), builtin()).unwrap(),
            PathBuf::from("/data/out")
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(
            expand_output_root("~/images", Path::new("/home/u"), builtin()).unwrap(),
            PathBuf::from("/home/u/images")
        );
        assert_eq!(
            expand_output_root("~", Path::new("/home/u"), builtin()).unwrap(),
            PathBuf::from("/home/u")
        );
        assert!(expand_output_root("~x", Path::new("/home/u"), builtin()).is_err());
    }

    #[test]
    fn relative_and_empty_are_rejected() {
        assert!(expand_output_root("out", Path::new("/home/u"), builtin()).is_err());
        assert!(expand_output_root("", Path::new("/home/u"), builtin()).is_err());
        assert!(expand_output_root("   ", Path::new("/home/u"), builtin()).is_err());
    }

    #[test]
    fn omitting_the_override_falls_back_to_the_configured_root() {
        assert_eq!(
            output_root(None, Path::new("/cfg/out"), Path::new("/home/u"), builtin()).unwrap(),
            PathBuf::from("/cfg/out")
        );
        assert_eq!(
            output_root(
                Some("/other"),
                Path::new("/cfg/out"),
                Path::new("/home/u"),
                builtin()
            )
            .unwrap(),
            PathBuf::from("/other")
        );
    }
}
