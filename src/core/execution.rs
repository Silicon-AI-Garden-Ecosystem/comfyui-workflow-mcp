//! One run is one directory of artifacts, and the path of that directory is the whole
//! answer. The server keeps no queue, no lock and no run state: ComfyUI serializes its
//! own queue, and a second scheduler could only ever disagree with it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value as Json};

use crate::config::Workflow;
use crate::core::comfy::{self, Client};
use crate::core::error::{ComfyError, ParamError, Stage};
use crate::core::output;
use crate::core::overrides;

#[derive(Debug)]
pub enum RunError {
    Params(ParamError),
    Comfy(ComfyError),
    Io { path: PathBuf, detail: String },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(e) => write!(f, "{e}"),
            Self::Comfy(e) => write!(f, "{e}"),
            Self::Io { path, detail } => write!(f, "{}: {}", path.display(), detail),
        }
    }
}

impl From<ParamError> for RunError {
    fn from(value: ParamError) -> Self {
        Self::Params(value)
    }
}

impl From<ComfyError> for RunError {
    fn from(value: ComfyError) -> Self {
        Self::Comfy(value)
    }
}

impl From<std::io::Error> for RunError {
    fn from(value: std::io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            detail: value.to_string(),
        }
    }
}

/// Only the directory comes back; a drawn value stays in the artifacts, not in the
/// response.
pub async fn execute(
    client: &Client,
    workflow: &Workflow,
    params: Option<&Map<String, Json>>,
    root: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<PathBuf, RunError> {
    let prompt = overrides::resolve(
        &workflow.manifest,
        &workflow.json,
        params,
        &mut overrides::os_rng(),
    )?;

    // Unique within one process only, so a nanosecond timestamp replaces a uuid.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    let client_id = format!("comfyui-workflow-mcp-{nanos:x}");

    // The server picks the path and writes the workflow before the submit: ComfyUI's
    // history disappears on restart, and a rejected submit must leave the evidence behind.
    let dir = output::create_run_dir(root, &workflow.name, SystemTime::now()).map_err(|err| {
        RunError::Io {
            path: root.to_path_buf(),
            detail: err.to_string(),
        }
    })?;
    write_workflow(&dir, &prompt)?;

    let submitted = match client.submit(&prompt, &client_id).await {
        Ok(submitted) => submitted,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), error = %err, "submit failed");
            return Err(err.into());
        }
    };
    tracing::info!(
        workflow = %workflow.name,
        prompt_id = %submitted.prompt_id,
        client_id = %client_id,
        dir = %dir.display(),
        "submitted prompt"
    );

    let entry = poll(client, &submitted.prompt_id, timeout, poll_interval).await?;

    let files = comfy::collect_files(&entry.outputs);
    tracing::info!(
        prompt_id = %submitted.prompt_id,
        files = files.len(),
        "execution finished"
    );

    let mut payloads = Vec::with_capacity(files.len());
    for file in &files {
        let bytes = client.download(file).await?;
        tracing::info!(
            filename = %file.filename,
            subfolder = %file.subfolder,
            kind = file.kind.as_str(),
            bytes = bytes.len(),
            "downloaded file"
        );
        payloads.push((file.filename.clone(), bytes));
    }

    let written = write_files(&dir, payloads).map_err(|err| RunError::Io {
        path: dir.clone(),
        detail: err.to_string(),
    })?;
    tracing::info!(
        dir = %dir.display(),
        written = written.len(),
        "run directory ready"
    );

    let dir = std::fs::canonicalize(&dir).map_err(|err| RunError::Io {
        path: dir.clone(),
        detail: err.to_string(),
    })?;
    Ok(dir)
}

async fn poll(
    client: &Client,
    prompt_id: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<comfy::HistoryEntry, ComfyError> {
    let deadline = Instant::now() + timeout;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        if let Some(entry) = client.history(prompt_id).await? {
            let has_outputs = entry
                .outputs
                .as_object()
                .is_some_and(|outputs| !outputs.is_empty());
            // ComfyUI can fill `outputs` before it marks the entry successful: both signals are watched.
            if has_outputs || entry.status.status_str == "success" {
                return Ok(entry);
            }
            if entry.status.status_str == "error" {
                let detail = entry.status.execution_error().unwrap_or_else(|| {
                    let fallback = serde_json::json!({
                        "status_str": entry.status.status_str,
                        "completed": entry.status.completed,
                        "messages": entry.status.messages,
                    });
                    serde_json::to_string_pretty(&fallback)
                        .unwrap_or_else(|_| entry.status.status_str.clone())
                });
                // ComfyUI's own text is passed through verbatim: rewording would hide the detail.
                return Err(ComfyError::Raw {
                    stage: Stage::Execution,
                    detail,
                });
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ComfyError::Timeout {
                prompt_id: prompt_id.to_string(),
                seconds: timeout.as_secs(),
            });
        }
        tracing::debug!(prompt_id, attempt, "still running, polling again");
        tokio::time::sleep(interval.min(remaining)).await;
    }
}

fn write_workflow(dir: &Path, prompt: &Json) -> Result<(), RunError> {
    let path = dir.join("workflow.json");
    let bytes = serde_json::to_vec_pretty(prompt).map_err(|err| RunError::Io {
        path: path.clone(),
        detail: err.to_string(),
    })?;
    std::fs::write(&path, bytes).map_err(|err| RunError::Io {
        path,
        detail: err.to_string(),
    })
}

fn write_files(dir: &Path, files: Vec<(String, Vec<u8>)>) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::with_capacity(files.len());
    for (index, (filename, bytes)) in files.into_iter().enumerate() {
        let ext = output::extension_of(&filename);
        // `create_new`: a collision must not silently overwrite an earlier run.
        written.push(output::write_slot(dir, index, &ext, &bytes)?);
    }
    Ok(written)
}
