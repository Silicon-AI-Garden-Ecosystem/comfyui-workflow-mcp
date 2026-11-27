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

        // A hook may not touch a parameter's own target,
        // and a `set` may not live inside a path the same hook removes:
        // both would make the outcome depend on the order the edits happen to run in.
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

    // `random` needs an explicit source, because the range is part of the contract:
    // an implicit default range would be semantics the author never wrote.
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
mod tests;
