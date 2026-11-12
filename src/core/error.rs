//! One error text per audience: our own pre-flight errors are worded so the model can fix
//! them, ComfyUI's errors are forwarded unchanged. Nothing re-wraps the text.

use std::fmt;
use std::path::Path;

use crate::core::messages::{Messages, builtin, fill};

/// A startup failure, anchored at a location the reader can open (`file:line`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub at: String,
    pub message: String,
}

impl LoadError {
    pub fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            message: message.into(),
        }
    }

    pub fn at_file(path: &Path) -> String {
        path.display().to_string()
    }

    pub fn at_line(path: &Path, line: usize) -> String {
        format!("{}:{}", path.display(), line)
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

impl std::error::Error for LoadError {}

/// Parameter validation failure: exactly the errors the model is able to fix.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamError {
    Missing {
        items: Vec<(String, Option<String>)>,
    },
    Unknown {
        unknown: Vec<String>,
        allowed: Vec<String>,
    },
    TypeMismatch {
        param: String,
        expected: &'static str,
        got: &'static str,
    },
    OutOfRange {
        param: String,
        min: f64,
        max: f64,
        got: f64,
    },
    NotInEnum {
        param: String,
        allowed: Vec<String>,
        got: String,
    },
    /// A defect in the workflow JSON, not in the model's arguments: only the author can fix it.
    BadTarget { param: String, pointer: String },
}

impl ParamError {
    /// The wording lives in `[messages]`; this only decides which template and which
    /// arguments. Lists share one separator, so a translation stays consistent.
    pub fn render(&self, messages: &Messages) -> String {
        let params = &messages.params;
        let separator = messages.list_separator.as_str();
        match self {
            Self::Missing { items } => {
                let list = items
                    .iter()
                    .map(|(name, description)| match description {
                        Some(description) => fill(
                            &params.missing_with_description,
                            &[("name", name), ("description", description)],
                        ),
                        None => fill(&params.missing_bare, &[("name", name)]),
                    })
                    .collect::<Vec<_>>()
                    .join(separator);
                fill(&params.missing, &[("list", &list)])
            }
            Self::Unknown { unknown, allowed } => {
                let names = unknown
                    .iter()
                    .map(|name| fill(&params.quoted, &[("value", name)]))
                    .collect::<Vec<_>>()
                    .join(separator);
                fill(
                    &params.unknown,
                    &[("names", &names), ("allowed", &allowed.join(separator))],
                )
            }
            Self::TypeMismatch {
                param,
                expected,
                got,
            } => fill(
                &params.type_mismatch,
                &[("param", param), ("expected", expected), ("got", got)],
            ),
            Self::OutOfRange {
                param,
                min,
                max,
                got,
            } => fill(
                &params.out_of_range,
                &[
                    ("param", param),
                    ("min", &min.to_string()),
                    ("max", &max.to_string()),
                    ("got", &got.to_string()),
                ],
            ),
            Self::NotInEnum {
                param,
                allowed,
                got,
            } => fill(
                &params.not_in_enum,
                &[
                    ("param", param),
                    ("allowed", &allowed.join(separator)),
                    ("got", got),
                ],
            ),
            Self::BadTarget { param, pointer } => fill(
                &params.bad_target,
                &[("param", param), ("pointer", pointer)],
            ),
        }
    }
}

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(builtin()))
    }
}

impl std::error::Error for ParamError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Submit,
    Execution,
    Download,
    Upload,
    Check,
}

impl Stage {
    pub fn label(self, messages: &Messages) -> &str {
        let comfy = &messages.comfy;
        match self {
            Self::Submit => &comfy.stage_submit,
            Self::Execution => &comfy.stage_execution,
            Self::Download => &comfy.stage_download,
            Self::Upload => &comfy.stage_upload,
            Self::Check => &comfy.stage_check,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComfyError {
    /// Network level, and the only error that names the address; the others are ComfyUI's own.
    Transport { url: String, detail: String },
    /// The deadline passed; ComfyUI usually finished anyway, its output directory holds the results.
    Timeout { prompt_id: String, seconds: u64 },
    /// ComfyUI's own error, forwarded verbatim: rewording it would hide the detail.
    Raw { stage: Stage, detail: String },
    /// Something in front of ComfyUI answered (a proxy's HTML page), not ComfyUI itself.
    HttpStatus {
        stage: Stage,
        status: u16,
        body: String,
    },
    /// ComfyUI has no such artifact: `/view` answered 404, and the body was empty.
    MissingFile { filename: String },
}

impl ComfyError {
    pub fn render(&self, messages: &Messages) -> String {
        let comfy = &messages.comfy;
        match self {
            Self::Transport { url, detail } => {
                fill(&comfy.transport, &[("url", url), ("detail", detail)])
            }
            Self::Timeout { prompt_id, seconds } => fill(
                &comfy.timeout,
                &[("seconds", &seconds.to_string()), ("prompt_id", prompt_id)],
            ),
            Self::Raw { stage, detail } => fill(
                &comfy.raw,
                &[("stage", stage.label(messages)), ("detail", detail)],
            ),
            Self::HttpStatus {
                stage,
                status,
                body,
            } => fill(
                &comfy.http_status,
                &[
                    ("stage", stage.label(messages)),
                    ("status", &status.to_string()),
                    ("body", body),
                ],
            ),
            Self::MissingFile { filename } => fill(
                &comfy.missing_file,
                &[
                    ("stage", Stage::Download.label(messages)),
                    ("filename", filename),
                ],
            ),
        }
    }
}

impl fmt::Display for ComfyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(builtin()))
    }
}

impl std::error::Error for ComfyError {}

/// A model-supplied path that cannot become an uploaded file: every variant names the
/// file the caller can fix, and nothing here talks about HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    NotAbsolute { raw: String },
    NoExtension { path: String },
    NotFound { path: String },
    NotAFile { path: String },
    Read { path: String, detail: String },
}

impl FileError {
    pub fn render(&self, messages: &Messages) -> String {
        let files = &messages.files;
        match self {
            Self::NotAbsolute { raw } => fill(&files.not_absolute, &[("raw", raw)]),
            Self::NoExtension { path } => fill(&files.no_extension, &[("path", path)]),
            Self::NotFound { path } => fill(&files.not_found, &[("path", path)]),
            Self::NotAFile { path } => fill(&files.not_a_file, &[("path", path)]),
            Self::Read { path, detail } => fill(&files.read, &[("path", path), ("detail", detail)]),
        }
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(builtin()))
    }
}

impl std::error::Error for FileError {}

/// Cut to 500 bytes: a proxy's HTML page must not become the model's context.
pub fn preview_body(body: &str) -> String {
    const LIMIT: usize = 500;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    let mut end = LIMIT;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &body[..end])
}
