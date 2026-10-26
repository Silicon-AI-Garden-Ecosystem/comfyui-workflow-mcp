use std::ops::Range;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value as Json;
use toml::Spanned;

use super::*;
use crate::core::error::LoadError;

impl Manifest {
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

        // A hook may not touch a parameter's own target, and a `set` may not live inside a
        // path the same hook removes: both would make the outcome depend on the order the
        // edits happen to run in.
        for (name, param) in &params {
            for hook in &param.hooks {
                for removed in &hook.remove {
                    for set in &hook.set {
                        if set.target.starts_with(removed.as_slice()) {
                            return Err(LoadError::new(
                                LoadError::at_file(src.path),
                                format!(
                                    "params.{name}.hook sets {} inside the removed {}; split them into two hooks",
                                    pointer(&set.target),
                                    pointer(removed)
                                ),
                            ));
                        }
                    }
                }
                let touched = hook.remove.iter().chain(hook.set.iter().map(|s| &s.target));
                for path in touched {
                    if let Some((other, _)) = params
                        .iter()
                        .find(|(_, p)| p.target.starts_with(path.as_slice()))
                    {
                        return Err(LoadError::new(
                            LoadError::at_file(src.path),
                            format!(
                                "params.{name}.hook touches {} which contains params.{other}'s target",
                                pointer(path)
                            ),
                        ));
                    }
                }
            }
        }

        Ok(Self {
            description: raw.description.get_ref().clone(),
            params,
        })
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
    #[serde(default)]
    hook: Vec<Spanned<RawHook>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHook {
    when: Spanned<String>,
    #[serde(default)]
    remove: Vec<Spanned<Vec<Spanned<String>>>>,
    #[serde(default)]
    set: Vec<Spanned<RawHookSet>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHookSet {
    target: Spanned<Vec<Spanned<String>>>,
    value: Spanned<Json>,
}

fn parse_param(src: &Source<'_>, name: &str, raw: &Spanned<RawParam>) -> Result<Param, LoadError> {
    let raw = raw.get_ref();
    let ty = ParamType::parse(raw.ty.get_ref()).ok_or_else(|| {
        src.at(
            raw.ty.span(),
            format!(
                "params.{name}.type = \"{}\" is invalid; expected string / integer / float / boolean / image",
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
    if random && ty == ParamType::File {
        let span = raw.default.as_ref().map(|sp| sp.span()).unwrap_or(0..0);
        return Err(src.at(
            span,
            format!("params.{name}.default = \"random\" is not valid for file parameters"),
        ));
    }
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

    if !raw.hook.is_empty() && raw.default.is_none() {
        let span = raw.hook.first().map(|h| h.span()).unwrap_or(0..0);
        return Err(src.at(
            span,
            format!(
                "params.{name}.hook needs default: the hook fires when the parameter is left at its default, so without one it could never run"
            ),
        ));
    }

    let mut hooks = Vec::with_capacity(raw.hook.len());
    for raw_hook in &raw.hook {
        let hook = raw_hook.get_ref();
        if hook.when.get_ref() != "default" {
            return Err(src.at(
                hook.when.span(),
                format!(
                    "params.{name}.hook.when = \"{}\" is not supported; expected \"default\"",
                    hook.when.get_ref()
                ),
            ));
        }
        if hook.remove.is_empty() && hook.set.is_empty() {
            return Err(src.at(
                raw_hook.span(),
                format!("params.{name}.hook needs remove or set"),
            ));
        }
        let mut remove = Vec::with_capacity(hook.remove.len());
        for path in &hook.remove {
            remove.push(path_segments(src, name, "hook.remove", path)?);
        }
        let mut set = Vec::with_capacity(hook.set.len());
        for entry in &hook.set {
            let entry = entry.get_ref();
            let target = path_segments(src, name, "hook.set", &entry.target)?;
            if !is_leaf_scalar(entry.value.get_ref()) {
                return Err(src.at(
                    entry.value.span(),
                    format!(
                        "params.{name}.hook.set value must be a scalar (got {})",
                        json_kind(entry.value.get_ref())
                    ),
                ));
            }
            set.push(HookSet {
                target,
                value: entry.value.get_ref().clone(),
            });
        }
        hooks.push(Hook {
            when: HookWhen::Default,
            remove,
            set,
        });
    }

    Ok(Param {
        target,
        ty,
        default,
        random,
        range,
        values,
        description: raw.description.as_ref().map(|d| d.get_ref().clone()),
        hooks,
    })
}

fn path_segments(
    src: &Source<'_>,
    name: &str,
    field: &str,
    segments: &Spanned<Vec<Spanned<String>>>,
) -> Result<Vec<String>, LoadError> {
    if segments.get_ref().is_empty() {
        return Err(src.at(
            segments.span(),
            format!("params.{name}.{field} must not be empty"),
        ));
    }
    let mut out = Vec::with_capacity(segments.get_ref().len());
    for segment in segments.get_ref() {
        if segment.get_ref().is_empty() {
            return Err(src.at(
                segment.span(),
                format!("params.{name}.{field} contains an empty path segment"),
            ));
        }
        out.push(segment.get_ref().clone());
    }
    Ok(out)
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
    use crate::config::manifest::test_support::*;
    use serde_json::json;

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
    fn a_hook_is_parsed_from_the_sample() {
        let m = parse(include_str!(
            "../../../sim/workflows/local/image_qwen_image_2_1_image_edit.toml"
        ))
        .unwrap();
        let image2 = &m.params["image_2"];
        assert_eq!(image2.hooks.len(), 1);
        assert_eq!(image2.hooks[0].when, HookWhen::Default);
        assert_eq!(
            image2.hooks[0].remove,
            vec![vec![
                "459:474".to_string(),
                "inputs".to_string(),
                "images.image_2".to_string()
            ]]
        );
        assert_eq!(image2.hooks[0].set.len(), 1);
        assert_eq!(image2.hooks[0].set[0].value, json!(false));
        // The default is the marker the hook fires on; it is never written to the JSON.
        assert!(!image2.required());
        assert_eq!(image2.default, Some(json!("")));
    }

    #[test]
    fn hook_when_must_be_default() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"always\"\nremove = [[\"3\", \"inputs\", \"steps\"]]\n",
        ));
        assert!(err.message.contains("is not supported"), "{}", err.message);
        assert!(err.message.contains("\"default\""), "{}", err.message);
    }

    #[test]
    fn a_hook_needs_an_action() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\n",
        ));
        assert!(
            err.message.contains("needs remove or set"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_hook_set_value_must_be_a_scalar() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\nset = [{ target = [\"3\", \"inputs\", \"steps\"], value = [1, 2] }]\n",
        ));
        assert!(err.message.contains("must be a scalar"), "{}", err.message);
    }

    #[test]
    fn a_hook_requires_a_default() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\", \"steps\"]]\n",
        ));
        assert!(
            err.message.contains("hook needs default"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_hook_may_not_touch_a_parameter_target() {
        let err = parse_err(
            "description = \"d\"\n\n[params.p]\ntarget = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\"]]\n",
        );
        assert!(
            err.message.contains("contains params.p's target"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_hook_set_may_not_live_inside_its_remove() {
        let err = parse_err(&param_toml(
            "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\", \"cfg\"]]\nset = [{ target = [\"3\", \"inputs\", \"cfg\", \"x\"], value = 1 }]\n",
        ));
        assert!(
            err.message.contains("inside the removed"),
            "{}",
            err.message
        );
    }
}
