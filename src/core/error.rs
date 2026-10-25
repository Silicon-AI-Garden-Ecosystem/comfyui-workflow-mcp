//! One error text per audience: our own pre-flight errors are worded so the model can fix
//! them, ComfyUI's errors are forwarded unchanged. Nothing re-wraps the text.

use std::fmt;
use std::path::Path;

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

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { items } => {
                write!(f, "missing required parameters: ")?;
                for (i, (name, description)) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match description {
                        Some(d) => write!(f, "{name} ({d})")?,
                        None => write!(f, "{name}")?,
                    }
                }
                Ok(())
            }
            Self::Unknown { unknown, allowed } => write!(
                f,
                "unknown parameter {}. Available: {}",
                quote_all(unknown),
                allowed.join(", ")
            ),
            Self::TypeMismatch {
                param,
                expected,
                got,
            } => write!(f, "parameter \"{param}\" expects {expected}, got {got}"),
            Self::OutOfRange {
                param,
                min,
                max,
                got,
            } => write!(
                f,
                "parameter \"{param}\" is out of range [{min}, {max}]: {got}"
            ),
            Self::NotInEnum {
                param,
                allowed,
                got,
            } => write!(
                f,
                "parameter \"{param}\" must be one of: {} (got \"{got}\")",
                allowed.join(", ")
            ),
            Self::BadTarget { param, pointer } => {
                write!(
                    f,
                    "parameter \"{param}\" targets {pointer}, which is not a writable leaf scalar"
                )
            }
        }
    }
}

impl std::error::Error for ParamError {}

fn quote_all(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Submit,
    Execution,
    Download,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Submit => "submit failed",
            Self::Execution => "execution failed",
            Self::Download => "download failed",
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

impl fmt::Display for ComfyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { url, detail } => write!(f, "ComfyUI unreachable at {url}: {detail}"),
            Self::Timeout { prompt_id, seconds } => write!(
                f,
                "still running after {seconds}s ({prompt_id}); outputs will land in ComfyUI's own output directory"
            ),
            Self::Raw { stage, detail } => write!(f, "{}\n{detail}", stage.label()),
            Self::HttpStatus {
                stage,
                status,
                body,
            } => write!(f, "{}\nHTTP {status}\n{body}", stage.label()),
            Self::MissingFile { filename } => {
                write!(
                    f,
                    "download failed\nComfyUI has no such output file: \"{filename}\""
                )
            }
        }
    }
}

impl std::error::Error for ComfyError {}

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
