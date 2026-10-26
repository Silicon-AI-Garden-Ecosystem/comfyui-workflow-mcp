//! The wire protocol is mirrored, not modelled: a submit counts as success only when
//! ComfyUI answered 200 with empty `node_errors`, and its error bodies are forwarded
//! unchanged (measured, see `docs/probe.md`). The address comes from `env.toml`, never from
//! the tool arguments.

use serde_json::Value as Json;

use crate::core::error::{ComfyError, Stage, preview_body};

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubmitOk {
    pub prompt_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub outputs: Json,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub status_str: String,
    pub completed: bool,
    pub messages: Vec<Json>,
}

impl Status {
    pub fn execution_error(&self) -> Option<String> {
        self.messages.iter().find_map(|msg| {
            let pair = msg.as_array()?;
            if pair.first()?.as_str()? != "execution_error" {
                return None;
            }
            Some(match pair.get(1) {
                Some(detail) => serde_json::to_string_pretty(detail).ok()?,
                None => msg.to_string(),
            })
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    Output,
    Temp,
}

impl RemoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Temp => "temp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub filename: String,
    pub subfolder: String,
    pub kind: RemoteKind,
}

impl Client {
    /// Rebuilt for every call rather than cached: the TLS behaviour rides on the URL
    /// scheme, so a client must not outlive a configuration change.
    pub fn new(base_url: &str) -> Result<Self, ComfyError> {
        // reqwest 0.13 panics in `build()` unless its crypto provider is installed;
        // installing it twice returns Err, which is harmless here.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut builder = reqwest::Client::builder().tls_backend_rustls();

        // reqwest loads the platform root certificates while building the client, even
        // for plain http: without /etc/ssl/certs (nix sandbox) that build fails outright.
        if !is_https(base_url) {
            builder = builder.tls_danger_accept_invalid_certs(true);
        }

        let http = builder.build().map_err(|err| ComfyError::Transport {
            url: base_url.to_string(),
            detail: error_chain(&err),
        })?;

        Ok(Self {
            http,
            base: base_url.to_string(),
        })
    }

    pub async fn system_stats(&self) -> Result<String, ComfyError> {
        let response = self
            .http
            .get(self.url("/system_stats"))
            .send()
            .await
            .map_err(|err| self.transport(err))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|err| self.transport(err))?;

        let value: Json = serde_json::from_str(&body).map_err(|_| ComfyError::HttpStatus {
            stage: Stage::Submit,
            status,
            body: preview_body(&body),
        })?;
        if status != 200 {
            return Err(json_error(Stage::Submit, status, &body));
        }
        let version = value
            .get("comfyui_version")
            .and_then(Json::as_str)
            .ok_or_else(|| ComfyError::Raw {
                stage: Stage::Submit,
                detail: pretty(&value),
            })?;
        Ok(format!("ComfyUI {version}"))
    }

    /// `HEAD` answers 200/404 with no body (measured), so a file that ComfyUI already
    /// holds costs one round trip and zero bytes; anything else means a proxy is in the way.
    pub async fn has_input(&self, name: &str) -> Result<bool, ComfyError> {
        let response = self
            .http
            .head(self.url("/view"))
            .query(&[("filename", name), ("type", "input")])
            .send()
            .await
            .map_err(|err| self.transport(err))?;

        match response.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ComfyError::HttpStatus {
                    stage: Stage::Check,
                    status,
                    body: preview_body(&body),
                })
            }
        }
    }

    /// The multipart part's filename is the only thing that decides the stored name; the
    /// `filename` form field is ignored (measured). `overwrite` keeps uploads idempotent,
    /// and the returned name is still authoritative: ComfyUI suffixes on a collision.
    pub async fn upload_input(&self, name: &str, bytes: Vec<u8>) -> Result<String, ComfyError> {
        let form = reqwest::multipart::Form::new()
            .text("type", "input")
            .text("overwrite", "true")
            .part(
                "image",
                reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()),
            );

        let response = self
            .http
            .post(self.url("/upload/image"))
            .multipart(form)
            .send()
            .await
            .map_err(|err| self.transport(err))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|err| self.transport(err))?;

        if status != 200 {
            return Err(json_error(Stage::Upload, status, &body));
        }

        let value: Json = serde_json::from_str(&body).map_err(|_| ComfyError::HttpStatus {
            stage: Stage::Upload,
            status,
            body: preview_body(&body),
        })?;
        let stored = value.get("name").and_then(Json::as_str);
        let subfolder = value
            .get("subfolder")
            .and_then(Json::as_str)
            .filter(|folder| !folder.is_empty());

        match (subfolder, stored) {
            (Some(folder), Some(stored)) => Ok(format!("{folder}/{stored}")),
            (None, Some(stored)) => Ok(stored.to_string()),
            _ => Err(ComfyError::Raw {
                stage: Stage::Upload,
                detail: pretty(&value),
            }),
        }
    }

    /// A 200 alone is not success: the measured rule is HTTP 200 *and* empty
    /// `node_errors`; a validation failure is a 400 whose body is forwarded unchanged.
    pub async fn submit(&self, prompt: &Json, client_id: &str) -> Result<SubmitOk, ComfyError> {
        let payload = serde_json::json!({ "prompt": prompt, "client_id": client_id });
        let response = self
            .http
            .post(self.url("/prompt"))
            .json(&payload)
            .send()
            .await
            .map_err(|err| self.transport(err))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|err| self.transport(err))?;

        match status {
            200 => {
                let value: Json =
                    serde_json::from_str(&body).map_err(|_| ComfyError::HttpStatus {
                        stage: Stage::Submit,
                        status,
                        body: preview_body(&body),
                    })?;
                let node_errors_empty = value
                    .get("node_errors")
                    .and_then(Json::as_object)
                    .is_some_and(|errors| errors.is_empty());
                let prompt_id = value.get("prompt_id").and_then(Json::as_str);
                match prompt_id {
                    Some(prompt_id) if node_errors_empty => Ok(SubmitOk {
                        prompt_id: prompt_id.to_string(),
                    }),
                    _ => Err(ComfyError::Raw {
                        stage: Stage::Submit,
                        detail: pretty(&value),
                    }),
                }
            }
            400 => Err(match serde_json::from_str::<Json>(&body) {
                Ok(value) => ComfyError::Raw {
                    stage: Stage::Submit,
                    detail: pretty(&value),
                },
                Err(_) => ComfyError::Raw {
                    stage: Stage::Submit,
                    detail: body,
                },
            }),
            _ => Err(json_error(Stage::Submit, status, &body)),
        }
    }

    /// An empty object means "still running", not an error.
    pub async fn history(&self, prompt_id: &str) -> Result<Option<HistoryEntry>, ComfyError> {
        let response = self
            .http
            // The id must be in the path: without it ComfyUI returns the whole history.
            .get(self.url(&format!("/history/{prompt_id}")))
            .send()
            .await
            .map_err(|err| self.transport(err))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|err| self.transport(err))?;

        if status != 200 {
            return Err(json_error(Stage::Execution, status, &body));
        }
        let value: Json = serde_json::from_str(&body).map_err(|_| ComfyError::HttpStatus {
            stage: Stage::Execution,
            status,
            body: preview_body(&body),
        })?;

        let Some(entry) = value.get(prompt_id) else {
            return Ok(None);
        };

        let outputs = entry
            .get("outputs")
            .cloned()
            .unwrap_or_else(|| Json::Object(serde_json::Map::new()));
        let status_value = entry.get("status");
        let entry_status = Status {
            status_str: status_value
                .and_then(|s| s.get("status_str"))
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            completed: status_value
                .and_then(|s| s.get("completed"))
                .and_then(Json::as_bool)
                .unwrap_or(false),
            messages: status_value
                .and_then(|s| s.get("messages"))
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default(),
        };

        Ok(Some(HistoryEntry {
            outputs,
            status: entry_status,
        }))
    }

    pub async fn download(&self, file: &RemoteFile) -> Result<Vec<u8>, ComfyError> {
        // Real ComfyUI file names contain non-ASCII characters, and a raw query would
        // 400 the request line; `.query()` percent-encodes them.
        let response = self
            .http
            .get(self.url("/view"))
            .query(&[
                ("filename", file.filename.as_str()),
                ("subfolder", file.subfolder.as_str()),
                ("type", file.kind.as_str()),
            ])
            .send()
            .await
            .map_err(|err| self.transport(err))?;
        let status = response.status().as_u16();

        // A 404 body is empty rather than JSON, so there is nothing to parse.
        if status == 404 {
            return Err(ComfyError::MissingFile {
                filename: file.filename.clone(),
            });
        }
        if status == 200 {
            return response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|err| self.transport(err));
        }

        let body = response.text().await.map_err(|err| self.transport(err))?;
        Err(ComfyError::HttpStatus {
            stage: Stage::Download,
            status,
            body: preview_body(&body),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn transport(&self, err: reqwest::Error) -> ComfyError {
        ComfyError::Transport {
            url: self.base.clone(),
            detail: err.to_string(),
        }
    }
}

/// Non-200 handling: a JSON body is forwarded verbatim, anything else is cut to 500
/// bytes so that a proxy's HTML page never becomes the model's context.
fn json_error(stage: Stage, status: u16, body: &str) -> ComfyError {
    match serde_json::from_str::<Json>(body) {
        Ok(value) => ComfyError::Raw {
            stage,
            detail: pretty(&value),
        },
        Err(_) => ComfyError::HttpStatus {
            stage,
            status,
            body: preview_body(body),
        },
    }
}

fn pretty(value: &Json) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `outputs` mixes images, dims and text; only objects with a `filename` are
/// downloadable, and `type=output` wins over `temp` so no image is fetched twice.
pub fn collect_files(outputs: &Json) -> Vec<RemoteFile> {
    let mut found = Vec::new();

    let Some(nodes) = outputs.as_object() else {
        return found;
    };
    for value in nodes.values() {
        match value {
            Json::Array(items) => collect_array(items, &mut found),
            Json::Object(inner) => {
                for array in inner.values().filter_map(Json::as_array) {
                    collect_array(array, &mut found);
                }
            }
            _ => {}
        }
    }

    if found.iter().any(|file| file.kind == RemoteKind::Output) {
        found.retain(|file| file.kind == RemoteKind::Output);
    }
    found
}

fn collect_array(items: &[Json], found: &mut Vec<RemoteFile>) {
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(filename) = object.get("filename").and_then(Json::as_str) else {
            continue;
        };
        let subfolder = object
            .get("subfolder")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        let kind = match object.get("type").and_then(Json::as_str) {
            Some("temp") => RemoteKind::Temp,
            _ => RemoteKind::Output,
        };
        found.push(RemoteFile {
            filename: filename.to_string(),
            subfolder,
            kind,
        });
    }
}

fn is_https(url: &str) -> bool {
    url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://")
}

/// reqwest's own Display is only "builder error" or "error sending request"; the
/// actionable detail (missing root certificates, connection refused, DNS) is in the chain.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(cause) = cur {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        cur = cause.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_files_skips_non_file_entries() {
        let outputs = serde_json::json!({
            "9": {"images": [
                {"filename": "a.png", "subfolder": "", "type": "output",
                 "metadata": {"kind": "image"}}
            ]},
            "34": {"dims": [512, 512]},
            "26": {"text": ["some text"]},
            "18:13": {"last_incoming": {"__value__": 1}},
            "5": "not-an-object"
        });
        let files = collect_files(&outputs);
        assert_eq!(
            files,
            vec![RemoteFile {
                filename: "a.png".to_string(),
                subfolder: String::new(),
                kind: RemoteKind::Output,
            }]
        );

        // A shape nobody expects must not panic, it must only yield nothing.
        assert!(collect_files(&serde_json::json!({})).is_empty());
        assert!(collect_files(&serde_json::json!([1, 2, 3])).is_empty());
        assert!(collect_files(&Json::Null).is_empty());
    }

    #[test]
    fn collect_files_prefers_output_over_temp() {
        let outputs = serde_json::json!({
            "1": {"images": [{"filename": "final.png", "type": "output"}]},
            "2": {"images": [{"filename": "preview.png", "type": "temp", "subfolder": "x"}]}
        });
        let files = collect_files(&outputs);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "final.png");
    }

    #[test]
    fn collect_files_falls_back_to_temp() {
        let outputs = serde_json::json!({
            "36": {"images": [{"filename": "ui.png", "type": "temp"}]},
            "26": {"text": ["layout json"]}
        });
        let files = collect_files(&outputs);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, RemoteKind::Temp);
        assert_eq!(files[0].subfolder, "");
    }
}
