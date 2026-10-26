//! The TOML half of a workflow pair is a contract for a human author, so it is read strictly:
//! an unknown key is an error instead of a silent no-op, and every error points at its line --
//! a contract that can be misread produces a run that differs from what the author wrote.

use std::collections::BTreeMap;

use serde_json::Value as Json;

mod parse;
mod validate;

/// The reserved `default` value: the server draws this run's value from `range` / `enum`, so
/// the drawn value is always one the workflow accepts.
pub const RANDOM: &str = "random";

/// Integer bounds are capped here: MCP clients parse JSON numbers as doubles, so a larger
/// value comes back changed -- a silently altered seed is worse than a rejected one.
pub const MAX_SAFE_INT: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    String,
    Integer,
    Float,
    Boolean,
    /// A path on the machine running this server: the file is uploaded to ComfyUI and
    /// replaced by a content-addressed name before the prompt is submitted.
    File,
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::File => "file",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "string" => Some(Self::String),
            "integer" => Some(Self::Integer),
            "float" => Some(Self::Float),
            "boolean" => Some(Self::Boolean),
            "file" => Some(Self::File),
            _ => None,
        }
    }

    /// A float accepts an integer: `8` and `8.0` are the same JSON number, so the declared
    /// type is the only thing that says which spelling is expected. Nothing else is converted.
    pub fn accepts(self, value: &Json) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Integer => is_integer(value),
            Self::Float => value.is_number(),
            Self::Boolean => value.is_boolean(),
            // The caller passes a path; the wire value is the uploaded file's name, also a string.
            Self::File => value.is_string(),
        }
    }
}

pub fn json_kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(n) if n.is_f64() => "float",
        Json::Number(_) => "integer",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

pub fn is_integer(value: &Json) -> bool {
    match value {
        Json::Number(n) => n.is_u64() || n.is_i64(),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// JSON pointer segments, e.g. `["6", "inputs", "text"]`: addressing is exact on purpose,
    /// because default titles repeat and a semantic match can write the positive prompt into
    /// the negative one.
    pub target: Vec<String>,
    pub ty: ParamType,
    pub default: Option<Json>,
    pub random: bool,
    /// A closed interval. The range is part of the contract, so an explicit value is checked
    /// against it just like a drawn one.
    pub range: Option<(f64, f64)>,
    pub values: Option<Vec<String>>,
    pub description: Option<String>,
    /// Edits that run when the caller leaves this parameter out. Declared by the template
    /// author, never chosen by the model: an optional input has to disappear from the graph
    /// as a whole (the link to it included), which no argument value can express.
    pub hooks: Vec<Hook>,
}

/// The trigger of a hook. Only "default" exists: the parameter was not passed, so the
/// workflow runs on the value the template was exported with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookWhen {
    Default,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub when: HookWhen,
    /// Paths whose key is deleted, which is how a link is disconnected in the API JSON.
    pub remove: Vec<Vec<String>>,
    pub set: Vec<HookSet>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookSet {
    pub target: Vec<String>,
    pub value: Json,
}

impl Param {
    /// Absent `default` means required; `default = "random"` counts as optional. A hook is
    /// not a third behaviour: it runs only on the default branch, so it still needs a default.
    pub fn required(&self) -> bool {
        self.default.is_none() && !self.random
    }
}

impl Param {
    pub fn pointer(&self) -> String {
        pointer(&self.target)
    }
}

pub(crate) fn pointer(path: &[String]) -> String {
    let mut out = String::new();
    for segment in path {
        out.push('/');
        out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub description: String,
    pub params: BTreeMap<String, Param>,
}

/// A target must be a writable leaf: in ComfyUI's export an array is a link, not a value, and
/// an object or null is not a scalar either.
pub(crate) fn is_leaf_scalar(value: &Json) -> bool {
    matches!(value, Json::Number(_) | Json::String(_) | Json::Bool(_))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use serde_json::{Value as Json, json};

    use super::Manifest;
    use crate::core::error::LoadError;

    pub(crate) const PATH: &str = "workflow.toml";

    pub(crate) fn path() -> &'static Path {
        Path::new(PATH)
    }

    pub(crate) fn parse(text: &str) -> Result<Manifest, LoadError> {
        Manifest::parse(text, path())
    }

    pub(crate) fn parse_err(text: &str) -> LoadError {
        parse(text).expect_err("must be rejected")
    }

    pub(crate) fn param_toml(body: &str) -> String {
        format!("description = \"d\"\n\n[params.p]\n{body}")
    }

    pub(crate) fn validate_json() -> Json {
        json!({
            "3": {"inputs": {"seed": 1, "steps": 20, "cfg": 8.0}},
            "4": {"inputs": {"model": ["1", 0]}},
            "6": {"inputs": {"text": "hi"}},
            "8": {"inputs": {"nothing": null}},
        })
    }
}
