//! One run is one directory of artifacts, and the path of that directory is the whole
//! answer. The server keeps no queue, no lock and no run state: ComfyUI serializes its
//! own queue, and a second scheduler could only ever disagree with it.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value as Json};

use tokio::task::JoinSet;

use crate::config::Workflow;
use crate::core::comfy::{self, Client};
use crate::core::error::{ComfyError, ParamError, Stage};
use crate::core::file;
use crate::core::output;
use crate::core::overrides;
use crate::core::overrides::FileSlot;

/// comfyui serializes its own queue; four uploads in flight is enough to hide latency
/// without flooding a server that may be running on the same machine.
const UPLOAD_CONCURRENCY: usize = 4;

#[derive(Debug)]
pub enum RunError {
    Params(ParamError),
    Comfy(ComfyError),
    /// Independent failures: pre-flight checks report all of them at once, not the first.
    Multiple(Vec<String>),
    /// The whole run is bounded by one deadline: a stalled connection must not outlive the
    /// configured limit just because it is not the polling loop.
    Timeout {
        stage: &'static str,
        seconds: u64,
    },
    /// The workflow JSON lost a field the manifest pointed at, which can only happen if the
    /// file changed after it was loaded.
    MissingField(String),
    Io {
        path: PathBuf,
        detail: String,
    },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(e) => write!(f, "{e}"),
            Self::Comfy(e) => write!(f, "{e}"),
            Self::Multiple(messages) => write!(f, "{}", messages.join("\n")),
            Self::Timeout { stage, seconds } => {
                write!(f, "timed out after {seconds}s while {stage}")
            }
            Self::MissingField(pointer) => {
                write!(f, "the workflow JSON has no field at {pointer}")
            }
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
    let deadline = Instant::now() + timeout;

    let overrides::Resolved {
        json: mut prompt,
        files,
    } = overrides::resolve(
        &workflow.manifest,
        &workflow.json,
        params,
        &mut overrides::os_rng(),
    )?;

    // Uploads run before the run directory exists: a missing or unreadable local file must
    // not leave an empty directory behind.
    let names = phase(
        deadline,
        timeout,
        "uploading files",
        upload_files(client, &files),
    )
    .await?;
    overrides::apply_files(&mut prompt, &names).map_err(RunError::MissingField)?;

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

    let submitted = match phase(
        deadline,
        timeout,
        "submitting the prompt",
        client.submit(&prompt, &client_id),
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), error = %err, "submit failed");
            return Err(err);
        }
    };
    tracing::info!(
        workflow = %workflow.name,
        prompt_id = %submitted.prompt_id,
        client_id = %client_id,
        dir = %dir.display(),
        "submitted prompt"
    );

    let entry = poll(
        client,
        &submitted.prompt_id,
        deadline,
        timeout,
        poll_interval,
    )
    .await?;

    let remote = comfy::collect_files(&entry.outputs);
    tracing::info!(
        prompt_id = %submitted.prompt_id,
        files = remote.len(),
        "execution finished"
    );

    let mut payloads = Vec::with_capacity(remote.len());
    for file in &remote {
        let bytes = phase(
            deadline,
            timeout,
            "downloading files",
            client.download(file),
        )
        .await?;
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

/// Uploading is content-addressed: identical bytes always get the same name, so one file
/// named by two parameters is hashed once and uploaded once. Hashing finishes before any
/// upload starts, so a local problem is reported without touching the network.
async fn upload_files(
    client: &Client,
    files: &[FileSlot],
) -> Result<Vec<(String, String)>, RunError> {
    let mut located = Vec::with_capacity(files.len());
    let mut failures = Vec::new();
    for slot in files {
        match file::locate(&slot.path) {
            Ok((path, name)) => located.push((slot.pointer.clone(), path, name)),
            Err(err) => failures.push(err.to_string()),
        }
    }
    if !failures.is_empty() {
        return Err(RunError::Multiple(failures));
    }

    let mut uploaded = Vec::new();
    for window in unique_files(&located).chunks(UPLOAD_CONCURRENCY) {
        let mut tasks = JoinSet::new();
        for (name, path) in window {
            let client = client.clone();
            let name = name.clone();
            let path = path.clone();
            tasks.spawn(async move { upload_one(&client, &path, &name).await });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(pair)) => uploaded.push(pair),
                Ok(Err(message)) => failures.push(message),
                Err(err) => failures.push(err.to_string()),
            }
        }
    }
    if !failures.is_empty() {
        return Err(RunError::Multiple(failures));
    }

    let mut stored_by_name: HashMap<String, String> = uploaded.into_iter().collect();
    let mut named = Vec::with_capacity(located.len());
    for (pointer, _, name) in located {
        match stored_by_name.remove(&name) {
            Some(stored) => named.push((pointer, stored)),
            None => failures.push(format!("the upload of {name} returned no name")),
        }
    }
    if failures.is_empty() {
        Ok(named)
    } else {
        Err(RunError::Multiple(failures))
    }
}

/// One entry per distinct content name, in the order the parameters asked for them.
fn unique_files(located: &[(String, PathBuf, String)]) -> Vec<(String, PathBuf)> {
    let mut unique: Vec<(String, PathBuf)> = Vec::with_capacity(located.len());
    for (_, path, name) in located {
        if !unique.iter().any(|(known, _)| known == name) {
            unique.push((name.clone(), path.clone()));
        }
    }
    unique
}

async fn upload_one(client: &Client, path: &Path, name: &str) -> Result<(String, String), String> {
    if client
        .has_input(name)
        .await
        .map_err(|err| err.to_string())?
    {
        return Ok((name.to_string(), name.to_string()));
    }

    let bytes = std::fs::read(path)
        .map_err(|err| format!("failed to read file {}: {err}", path.display()))?;
    let stored = client
        .upload_input(name, bytes)
        .await
        .map_err(|err| err.to_string())?;
    Ok((name.to_string(), stored))
}

async fn poll(
    client: &Client,
    prompt_id: &str,
    deadline: Instant,
    timeout: Duration,
    interval: Duration,
) -> Result<comfy::HistoryEntry, ComfyError> {
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

/// Every network phase shares one deadline: the configured limit is a promise about the
/// whole run, not about the polling loop alone.
async fn phase<T, E>(
    deadline: Instant,
    timeout: Duration,
    stage: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, RunError>
where
    E: Into<RunError>,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(remaining, future).await {
        Ok(result) => result.map_err(Into::into),
        Err(_) => Err(RunError::Timeout {
            stage,
            seconds: timeout.as_secs(),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    #[test]
    fn the_same_content_is_uploaded_once() {
        let located = vec![
            (
                "/6/inputs/image".to_string(),
                PathBuf::from("/a.png"),
                "abc.png".to_string(),
            ),
            (
                "/9/inputs/image".to_string(),
                PathBuf::from("/b.png"),
                "abc.png".to_string(),
            ),
            (
                "/7/inputs/image".to_string(),
                PathBuf::from("/c.png"),
                "def.png".to_string(),
            ),
        ];

        assert_eq!(
            unique_files(&located),
            vec![
                ("abc.png".to_string(), PathBuf::from("/a.png")),
                ("def.png".to_string(), PathBuf::from("/c.png")),
            ]
        );
    }

    #[tokio::test]
    async fn a_phase_past_the_deadline_names_the_stage() {
        let err = phase::<(), RunError>(
            Instant::now(),
            Duration::from_secs(300),
            "uploading files",
            pending(),
        )
        .await
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "timed out after 300s while uploading files"
        );
    }
}
