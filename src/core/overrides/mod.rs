//! Parameters are applied to a copy of the workflow by exact JSON pointer,
//! never by class_type and title:
//! default titles repeat,
//! so a semantic match can write the positive prompt into the negative one.

use serde_json::{Map, Number, Value as Json};

use crate::config::manifest::{
    HookWhen, Manifest, Param, ParamType, is_integer, is_leaf_scalar, json_kind, pointer,
};
use crate::core::error::ParamError;

/// A file parameter whose value is still a local path:
/// the name on ComfyUI's side only exists after the file is hashed and uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSlot {
    pub pointer: String,
    pub path: String,
}

/// The workflow JSON plus the file work that is still owed to it.
/// Deref keeps callers treating the result as the workflow itself:
/// the slots are metadata, not the payload.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub json: Json,
    pub files: Vec<FileSlot>,
}

impl std::ops::Deref for Resolved {
    type Target = Json;

    fn deref(&self) -> &Self::Target {
        &self.json
    }
}

/// Writes the names the server returned into the JSON.
/// A pointer that no longer resolves means the file changed after it was loaded:
/// rejecting the run is better than submitting a prompt that still holds a local path.
pub fn apply_files(json: &mut Json, named: &[(String, String)]) -> Result<(), String> {
    for (pointer, name) in named {
        match json.pointer_mut(pointer) {
            Some(slot) => *slot = Json::String(name.clone()),
            None => return Err(pointer.clone()),
        }
    }
    Ok(())
}

/// Two behaviours only:
/// a `default` makes a parameter optional, no `default` makes it required.
/// A third one (keep whatever the JSON holds),
/// would make the result depend on whether the caller happened to pass a value.
pub fn resolve(
    manifest: &Manifest,
    json: &Json,
    params: Option<&Map<String, Json>>,
    rng: &mut dyn FnMut() -> u64,
) -> Result<Resolved, ParamError> {
    let empty = Map::new();
    let params = params.unwrap_or(&empty);

    let mut unknown: Vec<String> = params
        .keys()
        .filter(|key| !manifest.params.contains_key(*key))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(ParamError::Unknown {
            unknown,
            allowed: manifest.params.keys().cloned().collect(),
        });
    }

    let missing: Vec<(String, Option<String>)> = manifest
        .params
        .iter()
        .filter(|(name, param)| param.required() && !params.contains_key(*name))
        .map(|(name, param)| (name.clone(), param.description.clone()))
        .collect();
    if !missing.is_empty() {
        return Err(ParamError::Missing { items: missing });
    }

    let mut out = json.clone();
    let mut files = Vec::new();
    for (name, param) in &manifest.params {
        match params.get(name) {
            Some(value) => {
                // An explicit value always beats a random draw, `0` included.
                if !param.ty.accepts(value) {
                    return Err(ParamError::TypeMismatch {
                        param: name.clone(),
                        expected: param.ty.as_str(),
                        got: json_kind(value),
                    });
                }
                if param.ty == ParamType::File {
                    files.push(FileSlot {
                        pointer: param.pointer(),
                        path: value.as_str().unwrap_or_default().to_string(),
                    });
                    continue;
                }
                let value = coerce(param.ty, value);
                check_range(name, param, &value)?;
                check_enum(name, param, &value)?;
                write(&mut out, name, param, value)?;
            }
            None if param.random => {
                // The draw stays inside the declared range or enum:
                // it must be a value the workflow accepts.
                let value = sample(name, param, rng)?;
                write(&mut out, name, param, value)?;
            }
            None => {
                // The default is applied like any other value,
                // hooks run afterwards and may delete or overwrite what it wrote.
                // An empty file path means "no file".
                if let Some(default) = &param.default {
                    if param.ty == ParamType::File {
                        let path = default.as_str().unwrap_or_default();
                        if !path.is_empty() {
                            files.push(FileSlot {
                                pointer: param.pointer(),
                                path: path.to_string(),
                            });
                        }
                    } else {
                        write(&mut out, name, param, default.clone())?;
                    }
                }
            }
        }
    }

    for (name, param) in &manifest.params {
        if params.contains_key(name) {
            continue;
        }
        for hook in &param.hooks {
            match hook.when {
                HookWhen::Default => {}
            }
            for path in &hook.remove {
                if !remove_at(&mut out, path) {
                    return Err(ParamError::BadTarget {
                        param: name.clone(),
                        pointer: pointer(path),
                    });
                }
            }
            for entry in &hook.set {
                match out.pointer_mut(&pointer(&entry.target)) {
                    Some(slot) => *slot = entry.value.clone(),
                    None => {
                        return Err(ParamError::BadTarget {
                            param: name.clone(),
                            pointer: pointer(&entry.target),
                        });
                    }
                }
            }
        }
    }

    Ok(Resolved { json: out, files })
}

// Deleting the key is how a link is disconnected in the API JSON.
// There is no "set it to nothing":
// TOML cannot even spell null, and ComfyUI would still read the key as a value.
fn remove_at(json: &mut Json, path: &[String]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut cursor = json;
    for segment in parents {
        match cursor.get_mut(segment) {
            Some(next) => cursor = next,
            None => return false,
        }
    }
    match cursor {
        Json::Object(map) => map.remove(last).is_some(),
        _ => false,
    }
}

fn coerce(ty: ParamType, value: &Json) -> Json {
    match ty {
        ParamType::Float if is_integer(value) => number(value.as_f64().unwrap_or(0.0)),
        _ => value.clone(),
    }
}

// `serde_json` cannot hold infinity or NaN,
// so a non-finite float becomes `0`:
// one bad number must not sink the whole submit.
fn number(value: f64) -> Json {
    Json::Number(Number::from_f64(value).unwrap_or_else(|| Number::from(0i64)))
}

fn check_range(name: &str, param: &Param, value: &Json) -> Result<(), ParamError> {
    let Some((min, max)) = param.range else {
        return Ok(());
    };
    let got = value.as_f64().unwrap_or(f64::NAN);
    if got >= min && got <= max {
        Ok(())
    } else {
        Err(ParamError::OutOfRange {
            param: name.to_string(),
            min,
            max,
            got,
        })
    }
}

fn check_enum(name: &str, param: &Param, value: &Json) -> Result<(), ParamError> {
    let Some(values) = &param.values else {
        return Ok(());
    };
    let got = value.as_str().unwrap_or_default();
    if values.iter().any(|v| v == got) {
        Ok(())
    } else {
        Err(ParamError::NotInEnum {
            param: name.to_string(),
            allowed: values.clone(),
            got: got.to_string(),
        })
    }
}

fn write(out: &mut Json, name: &str, param: &Param, value: Json) -> Result<(), ParamError> {
    let pointer = param.pointer();
    let target = out
        .pointer_mut(&pointer)
        .ok_or_else(|| ParamError::BadTarget {
            param: name.to_string(),
            pointer: pointer.clone(),
        })?;
    // A ComfyUI link is an array, not a value: writing into one would corrupt the graph.
    if !is_leaf_scalar(target) {
        return Err(ParamError::BadTarget {
            param: name.to_string(),
            pointer,
        });
    }
    *target = value;
    Ok(())
}

fn sample(name: &str, param: &Param, rng: &mut dyn FnMut() -> u64) -> Result<Json, ParamError> {
    match param.ty {
        ParamType::Integer => {
            // The range is part of the contract, so it is required:
            // a random parameter without one never reaches a run.
            let Some((min, max)) = param.range else {
                return Err(ParamError::TypeMismatch {
                    param: name.to_string(),
                    expected: "integer with range",
                    got: "integer without range",
                });
            };
            let lo = min.ceil() as i64;
            let hi = max.floor() as i64;
            let span = (hi as i128 - lo as i128 + 1).max(1) as u128;
            let offset = (rng() as u128) % span;
            let value = lo as i128 + offset as i128;
            i64::try_from(value)
                .map(Json::from)
                .map_err(|_| ParamError::OutOfRange {
                    param: name.to_string(),
                    min,
                    max,
                    got: value as f64,
                })
        }
        ParamType::Float => {
            let Some((min, max)) = param.range else {
                return Err(ParamError::TypeMismatch {
                    param: name.to_string(),
                    expected: "float with range",
                    got: "float without range",
                });
            };
            // The draw must fit the 53-bit mantissa of an f64; the extra low bits are dropped.
            let unit = (rng() >> 11) as f64 / (1u64 << 53) as f64;
            let value = min + unit * (max - min);
            Ok(number(value))
        }
        ParamType::String => {
            let values = param
                .values
                .as_ref()
                .filter(|values| !values.is_empty())
                .ok_or_else(|| ParamError::TypeMismatch {
                    param: name.to_string(),
                    expected: "string with enum",
                    got: "string without enum",
                })?;
            let index = (rng() as usize) % values.len();
            Ok(Json::from(values[index].clone()))
        }
        // Rejected at load time; this arm only exists so the function cannot panic.
        ParamType::Boolean | ParamType::File => Err(ParamError::TypeMismatch {
            param: name.to_string(),
            expected: "integer / float / string",
            got: param.ty.as_str(),
        }),
    }
}

/// The bytes come from `/dev/urandom`,
/// if it cannot be opened the clock is used instead,
/// because degraded randomness beats failing the whole run.
pub fn os_rng() -> impl FnMut() -> u64 {
    use std::io::Read;

    move || {
        let mut buf = [0u8; 8];
        match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
            Ok(()) => u64::from_le_bytes(buf),
            Err(_) => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1),
        }
    }
}

#[cfg(test)]
mod tests;
