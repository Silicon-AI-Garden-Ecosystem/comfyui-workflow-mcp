//! The TOML half of a workflow pair is a contract for a human author, so it is read strictly:
//! an unknown key is an error instead of a silent no-op, and every error points at its line --
//! a contract that can be misread produces a run that differs from what the author wrote.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value as Json;
use toml::Spanned;

use crate::core::error::LoadError;

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
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "string" => Some(Self::String),
            "integer" => Some(Self::Integer),
            "float" => Some(Self::Float),
            "boolean" => Some(Self::Boolean),
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
}

impl Param {
    /// Absent `default` means required; `default = "random"` counts as optional. Only these two
    /// exist: a third would make the result depend on whether the caller passed a value.
    pub fn required(&self) -> bool {
        self.default.is_none() && !self.random
    }
}

impl Param {
    pub fn pointer(&self) -> String {
        let mut out = String::new();
        for segment in &self.target {
            out.push('/');
            out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
        }
        out
    }
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

impl Manifest {
    /// Everything that can be decided from the TOML alone. The checks that need the workflow
    /// JSON happen in `validate`, so one round reports every problem instead of one per fix.
    pub fn parse(text: &str, path: &Path) -> Result<Self, LoadError> {
        let src = Source::new(text, path);
        let raw: RawManifest = toml::from_str(text).map_err(|err| match err.span() {
            Some(span) => src.at(span, err.message()),
            None => LoadError::new(LoadError::at_file(path), err.message()),
        })?;

        if raw.description.get_ref().trim().is_empty() {
            return Err(src.at(raw.description.span(), "description must not be empty"));
        }

        let mut params = BTreeMap::new();
        for (name, raw_param) in &raw.params {
            params.insert(name.clone(), parse_param(&src, name, raw_param)?);
        }

        Ok(Self {
            description: raw.description.get_ref().clone(),
            params,
        })
    }

    /// The checks that need the workflow JSON: the target must exist, be a writable leaf and
    /// match the declared type. Every problem is collected, so one round reports them all; the
    /// spans are gone here, so these errors carry the file but no line.
    pub fn validate(&self, json: &Json, path: &Path) -> Result<(), Vec<LoadError>> {
        let at = LoadError::at_file(path);
        let mut errors = Vec::new();

        for (name, param) in &self.params {
            let pointer = param.pointer();
            let Some(value) = json.pointer(&pointer) else {
                errors.push(LoadError::new(
                    at.clone(),
                    format!("params.{name}.target={pointer} does not exist in the workflow JSON"),
                ));
                continue;
            };

            if !is_leaf_scalar(value) {
                let what = match value {
                    Json::Array(_) => "an array (a link, not a parameter)",
                    Json::Object(_) => "an object",
                    Json::Null => "null",
                    _ => unreachable!("is_leaf_scalar excludes number/string/bool"),
                };
                errors.push(LoadError::new(
                    at.clone(),
                    format!("params.{name}.target={pointer} points to {what}; it must point to a leaf scalar"),
                ));
                continue;
            }

            if !param.ty.accepts(value) {
                errors.push(LoadError::new(
                    at.clone(),
                    format!(
                        "params.{name}.type = {} but {pointer} is {} in the workflow JSON; they are incompatible",
                        param.ty.as_str(),
                        json_kind(value)
                    ),
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

struct Source<'a> {
    path: &'a Path,
    starts: Vec<usize>,
}

impl<'a> Source<'a> {
    fn new(text: &'a str, path: &'a Path) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { path, starts }
    }

    fn line(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    fn at(&self, span: impl Into<Range<usize>>, message: impl Into<String>) -> LoadError {
        let span = span.into();
        LoadError::new(
            LoadError::at_line(self.path, self.line(span.start)),
            message,
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    description: Spanned<String>,
    #[serde(default)]
    params: BTreeMap<String, Spanned<RawParam>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    target: Spanned<Vec<Spanned<String>>>,
    #[serde(rename = "type")]
    ty: Spanned<String>,
    #[serde(default)]
    default: Option<Spanned<Json>>,
    #[serde(default)]
    range: Option<Spanned<Vec<Spanned<Json>>>>,
    #[serde(default, rename = "enum")]
    values: Option<Spanned<Vec<Spanned<Json>>>>,
    #[serde(default)]
    description: Option<Spanned<String>>,
}

fn parse_param(src: &Source<'_>, name: &str, raw: &Spanned<RawParam>) -> Result<Param, LoadError> {
    let raw = raw.get_ref();
    let ty = ParamType::parse(raw.ty.get_ref()).ok_or_else(|| {
        src.at(
            raw.ty.span(),
            format!(
                "params.{name}.type = \"{}\" is invalid; expected string / integer / float / boolean",
                raw.ty.get_ref()
            ),
        )
    })?;

    if raw.target.get_ref().is_empty() {
        return Err(src.at(
            raw.target.span(),
            format!("params.{name}.target must not be empty"),
        ));
    }
    let mut target = Vec::with_capacity(raw.target.get_ref().len());
    for segment in raw.target.get_ref().iter() {
        if segment.get_ref().is_empty() {
            return Err(src.at(
                segment.span(),
                format!("params.{name}.target contains an empty path segment"),
            ));
        }
        target.push(segment.get_ref().clone());
    }

    let random = raw
        .default
        .as_ref()
        .is_some_and(|sp| sp.get_ref().as_str() == Some(RANDOM));

    if raw.range.is_some() && raw.values.is_some() {
        let span = raw.values.as_ref().map(|sp| sp.span()).unwrap_or(0..0);
        return Err(src.at(
            span,
            format!("params.{name} cannot declare both range and enum"),
        ));
    }

    let range = match &raw.range {
        None => None,
        Some(sp) => {
            if !matches!(ty, ParamType::Integer | ParamType::Float) {
                return Err(src.at(
                    sp.span(),
                    format!(
                        "params.{name}.range is only valid for numeric parameters, not {}",
                        ty.as_str()
                    ),
                ));
            }
            if sp.get_ref().len() != 2 {
                return Err(src.at(sp.span(), format!("params.{name}.range must be [min, max]")));
            }
            let min = number_at(src, name, "range", &sp.get_ref()[0])?;
            let max = number_at(src, name, "range", &sp.get_ref()[1])?;
            if ty == ParamType::Integer {
                for bound in [&sp.get_ref()[0], &sp.get_ref()[1]] {
                    let Some(n) = bound.get_ref().as_i64() else {
                        return Err(src.at(
                            bound.span(),
                            format!(
                                "params.{name}.range on an integer parameter needs integer bounds (got {})",
                                json_kind(bound.get_ref())
                            ),
                        ));
                    };
                    if n.abs() > MAX_SAFE_INT {
                        return Err(src.at(
                            bound.span(),
                            format!(
                                "params.{name}.range bound {n} exceeds the safe-integer limit {MAX_SAFE_INT}: MCP clients parse JSON numbers as f64, so larger values come back changed"
                            ),
                        ));
                    }
                }
            }
            if !matches!(
                min.partial_cmp(&max),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ) {
                return Err(src.at(
                    sp.span(),
                    format!("params.{name}.range min ({min}) is greater than max ({max})"),
                ));
            }
            Some((min, max))
        }
    };

    let values = match &raw.values {
        None => None,
        Some(sp) => {
            if ty != ParamType::String {
                return Err(src.at(
                    sp.span(),
                    format!(
                        "params.{name}.enum is only valid for string parameters, not {}",
                        ty.as_str()
                    ),
                ));
            }
            let mut out = Vec::with_capacity(sp.get_ref().len());
            for value in sp.get_ref() {
                match value.get_ref().as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(src.at(
                            value.span(),
                            format!(
                                "params.{name}.enum values must be strings (got {})",
                                json_kind(value.get_ref())
                            ),
                        ));
                    }
                }
            }
            Some(out)
        }
    };

    // `random` needs an explicit source, because the range is part of the contract: an
    // implicit default range would be semantics the author never wrote.
    if random && range.is_none() && values.is_none() {
        let span = raw.default.as_ref().map(|sp| sp.span()).unwrap_or(0..0);
        return Err(src.at(
            span,
            format!(
                "params.{name}.default = \"random\" needs range (numeric) or enum (string) to draw from"
            ),
        ));
    }

    let default = match &raw.default {
        None => None,
        Some(_) if random => None,
        Some(sp) => {
            if !ty.accepts(sp.get_ref()) {
                return Err(src.at(
                    sp.span(),
                    format!(
                        "params.{name}.default does not match type = {} (got {})",
                        ty.as_str(),
                        json_kind(sp.get_ref())
                    ),
                ));
            }
            let mut value = sp.get_ref().clone();
            if ty == ParamType::Float && is_integer(&value) {
                value = Json::from(value.as_f64().unwrap_or(0.0));
            }
            if let Some((min, max)) = range {
                let got = value.as_f64().unwrap_or(f64::NAN);
                if !(got >= min && got <= max) {
                    return Err(src.at(
                        sp.span(),
                        format!("params.{name}.default = {got} is outside range [{min}, {max}]"),
                    ));
                }
            }
            if let Some(values) = &values {
                let got = value.as_str().unwrap_or_default();
                if !values.iter().any(|v| v == got) {
                    return Err(src.at(
                        sp.span(),
                        format!(
                            "params.{name}.default = \"{got}\" is not in enum (allowed: {})",
                            values.join(", ")
                        ),
                    ));
                }
            }
            Some(value)
        }
    };

    Ok(Param {
        target,
        ty,
        default,
        random,
        range,
        values,
        description: raw.description.as_ref().map(|d| d.get_ref().clone()),
    })
}

fn number_at(
    src: &Source<'_>,
    name: &str,
    field: &str,
    value: &Spanned<Json>,
) -> Result<f64, LoadError> {
    match value.get_ref().as_f64() {
        Some(n) => Ok(n),
        None => Err(src.at(
            value.span(),
            format!(
                "params.{name}.{field} contains {}, which is not a number",
                json_kind(value.get_ref())
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PATH: &str = "workflow.toml";

    fn path() -> &'static Path {
        Path::new(PATH)
    }

    fn parse(text: &str) -> Result<Manifest, LoadError> {
        Manifest::parse(text, path())
    }

    fn parse_err(text: &str) -> LoadError {
        parse(text).expect_err("must be rejected")
    }

    fn param_toml(body: &str) -> String {
        format!("description = \"d\"\n\n[params.p]\n{body}")
    }

    fn validate_json() -> Json {
        json!({
            "3": {"inputs": {"seed": 1, "steps": 20, "cfg": 8.0}},
            "4": {"inputs": {"model": ["1", 0]}},
            "6": {"inputs": {"text": "hi"}},
            "8": {"inputs": {"nothing": null}},
        })
    }

    #[test]
    fn parses_and_validates_sample_workflows() {
        let samples = [
            (
                include_str!("../../sim/workflows/image_sdxl_simple.toml"),
                include_str!("../../sim/workflows/image_sdxl_simple.json"),
            ),
            (
                include_str!("../../sim/workflows/image_qwen_image_2_1_t2i.toml"),
                include_str!("../../sim/workflows/image_qwen_image_2_1_t2i.json"),
            ),
        ];
        for (toml_text, json_text) in samples {
            let manifest = parse(toml_text).expect("the sample must parse");
            assert!(!manifest.params.is_empty());
            let json: Json =
                serde_json::from_str(json_text).expect("the sample JSON must be valid");
            manifest
                .validate(&json, path())
                .expect("the sample must validate");
        }

        // The shipped samples must keep the shape the tool promises: a parameter without
        // a default is required, a random one is optional.
        let sdxl = parse(include_str!("../../sim/workflows/image_sdxl_simple.toml")).unwrap();
        assert!(sdxl.params["prompt"].required());
        assert!(!sdxl.params["negative_prompt"].required());
        assert!(sdxl.params["seed"].random);
        assert_eq!(sdxl.params["seed"].default, None);
        assert!(sdxl.params["width"].default.is_some());
    }

    #[test]
    fn params_may_be_empty_or_absent() {
        let m = parse("description = \"d\"\n").expect("params is optional");
        assert!(m.params.is_empty());
        let m = parse("description = \"d\"\nparams = {}\n").expect("params may be an empty table");
        assert!(m.params.is_empty());
    }

    #[test]
    fn rejects_unknown_keys_with_position() {
        let err = parse_err("description = \"d\"\nextra = 1\n");
        assert!(err.message.contains("extra"), "{}", err.message);
        assert_eq!(err.at, "workflow.toml:2");

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nextra = true\n",
        ));
        assert!(err.message.contains("extra"), "{}", err.message);
        assert!(err.at.starts_with("workflow.toml:"), "{}", err.at);
    }

    #[test]
    fn description_must_be_present_and_non_empty() {
        let err = parse_err("params = {}\n");
        assert!(err.message.contains("description"), "{}", err.message);

        let err = parse_err("description = \"  \"\n");
        assert!(
            err.message.contains("description must not be empty"),
            "{}",
            err.message
        );
    }

    #[test]
    fn target_must_be_non_empty_segments_of_strings() {
        let err = parse_err(&param_toml("target = []\ntype = \"integer\"\n"));
        assert!(
            err.message.contains("target must not be empty"),
            "{}",
            err.message
        );
        assert_eq!(err.at, "workflow.toml:4");

        let err = parse_err(&param_toml("target = [\"3\", \"\"]\ntype = \"integer\"\n"));
        assert!(
            err.message.contains("empty path segment"),
            "{}",
            err.message
        );

        let err = parse_err(&param_toml("target = [\"3\", 4]\ntype = \"integer\"\n"));
        assert!(err.message.contains("invalid type"), "{}", err.message);
    }

    #[test]
    fn type_must_be_one_of_four() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"int\"\n",
        ));
        assert!(err.message.contains("is invalid"), "{}", err.message);
        assert!(err.message.contains("string / integer / float / boolean"));
        assert_eq!(err.at, "workflow.toml:5");
    }

    #[test]
    fn required_key_is_no_longer_accepted() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrequired = true\n",
        ));
        assert!(err.message.contains("unknown field"), "{}", err.message);
        assert_eq!(err.at, "workflow.toml:6");
    }

    #[test]
    fn integer_range_must_fit_safe_integers() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\nrange = [0, 9223372036854775807]\n",
        ));
        assert!(
            err.message.contains("safe-integer limit"),
            "{}",
            err.message
        );

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\nrange = [0.5, 10.5]\n",
        ));
        assert!(
            err.message.contains("needs integer bounds"),
            "{}",
            err.message
        );
    }

    #[test]
    fn range_must_be_two_numbers_on_numeric_params() {
        let err = parse_err(&param_toml(
            "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nrange = [1, 2]\n",
        ));
        assert!(
            err.message.contains("range is only valid for numeric"),
            "{}",
            err.message
        );

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [1]\n",
        ));
        assert!(
            err.message.contains("must be [min, max]"),
            "{}",
            err.message
        );

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [5, 1]\n",
        ));
        assert!(err.message.contains("greater than"), "{}", err.message);

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [\"a\", 1]\n",
        ));
        assert!(err.message.contains("is not a number"), "{}", err.message);
    }

    #[test]
    fn enum_only_on_strings_with_string_values() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nenum = [\"a\"]\n",
        ));
        assert!(
            err.message.contains("enum is only valid for"),
            "{}",
            err.message
        );

        let err = parse_err(&param_toml(
            "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nenum = [\"a\", 2]\n",
        ));
        assert!(err.message.contains("must be strings"), "{}", err.message);

        let err = parse_err(&param_toml(
            "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nrange = [1, 2]\nenum = [\"a\"]\n",
        ));
        assert!(err.message.contains("range and enum"), "{}", err.message);
    }

    #[test]
    fn default_must_match_type() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = \"28\"\n",
        ));
        assert!(err.message.contains("does not match"), "{}", err.message);
        assert!(err.message.contains("string"), "{}", err.message);

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = 1.5\n",
        ));
        assert!(err.message.contains("does not match"), "{}", err.message);

        let err = parse_err(&param_toml(
            "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\ndefault = 1\n",
        ));
        assert!(err.message.contains("does not match"), "{}", err.message);
    }

    #[test]
    fn float_default_accepts_integer_and_normalizes() {
        let m = parse(&param_toml(
            "target = [\"3\", \"inputs\", \"cfg\"]\ntype = \"float\"\ndefault = 8\n",
        ))
        .expect("a float accepts an integer");
        let default = m.params["p"].default.clone().expect("has a default");
        assert_eq!(default, json!(8.0));
        assert!(default.is_f64(), "must normalize to 8.0: {default}");
    }

    #[test]
    fn default_must_be_inside_range_and_enum() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = 200\nrange = [1, 100]\n",
        ));
        assert!(err.message.contains("outside range"), "{}", err.message);

        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"sampler_name\"]\ntype = \"string\"\ndefault = \"x\"\nenum = [\"a\", \"b\"]\n",
        ));
        assert!(err.message.contains("is not in enum"), "{}", err.message);
    }

    #[test]
    fn validate_checks_pointer_existence_and_leaf_scalars() {
        let json = validate_json();

        let m = parse(&param_toml(
            "target = [\"9\", \"inputs\", \"x\"]\ntype = \"integer\"\n",
        ))
        .unwrap();
        let errors = m.validate(&json, path()).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].message.contains("does not exist"),
            "{}",
            errors[0].message
        );
        assert_eq!(errors[0].at, PATH);

        let m = parse(&param_toml(
            "target = [\"4\", \"inputs\", \"model\"]\ntype = \"string\"\n",
        ))
        .unwrap();
        let errors = m.validate(&json, path()).unwrap_err();
        assert!(
            errors[0].message.contains("a link, not a parameter"),
            "{}",
            errors[0].message
        );

        let m = parse(&param_toml(
            "target = [\"3\", \"inputs\"]\ntype = \"string\"\n",
        ))
        .unwrap();
        let errors = m.validate(&json, path()).unwrap_err();
        assert!(
            errors[0].message.contains("an object"),
            "{}",
            errors[0].message
        );

        let m = parse(&param_toml(
            "target = [\"8\", \"inputs\", \"nothing\"]\ntype = \"string\"\n",
        ))
        .unwrap();
        let errors = m.validate(&json, path()).unwrap_err();
        assert!(errors[0].message.contains("null"), "{}", errors[0].message);
    }

    #[test]
    fn validate_checks_type_compatibility() {
        let json = validate_json();

        let m = parse(&param_toml(
            "target = [\"6\", \"inputs\", \"text\"]\ntype = \"integer\"\n",
        ))
        .unwrap();
        let errors = m.validate(&json, path()).unwrap_err();
        assert!(
            errors[0].message.contains("incompatible"),
            "{}",
            errors[0].message
        );

        let m = parse(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"float\"\n",
        ))
        .unwrap();
        assert!(
            m.validate(&json, path()).is_ok(),
            "an integer is valid for a float field"
        );
    }

    #[test]
    fn validate_collects_every_error() {
        let text = param_toml("target = [\"9\", \"inputs\", \"x\"]\ntype = \"integer\"\n")
            + "\n[params.b]\ntarget = [\"4\", \"inputs\", \"model\"]\ntype = \"string\"\n";
        let m = parse(&text).unwrap();
        let errors = m.validate(&validate_json(), path()).unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
    }
}
