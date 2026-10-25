//! Parameters are applied to a copy of the workflow by exact JSON pointer, never by
//! class_type and title: default titles repeat, so a semantic match can write the
//! positive prompt into the negative one.

use serde_json::{Map, Number, Value as Json};

use crate::config::manifest::{Manifest, Param, ParamType, is_integer, is_leaf_scalar, json_kind};
use crate::core::error::ParamError;

/// Two behaviours only: a `default` makes a parameter optional, no `default` makes it
/// required. A third one (keep whatever the JSON holds) would make the result depend on
/// whether the caller happened to pass a value.
pub fn resolve(
    manifest: &Manifest,
    json: &Json,
    params: Option<&Map<String, Json>>,
    rng: &mut dyn FnMut() -> u64,
) -> Result<Json, ParamError> {
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
                let value = coerce(param.ty, value);
                check_range(name, param, &value)?;
                check_enum(name, param, &value)?;
                write(&mut out, name, param, value)?;
            }
            None if param.random => {
                // The draw stays inside the declared range or enum: it must be a value
                // the workflow accepts.
                let value = sample(name, param, rng)?;
                write(&mut out, name, param, value)?;
            }
            None => {
                if let Some(default) = &param.default {
                    write(&mut out, name, param, default.clone())?;
                }
            }
        }
    }

    Ok(out)
}

fn coerce(ty: ParamType, value: &Json) -> Json {
    match ty {
        ParamType::Float if is_integer(value) => number(value.as_f64().unwrap_or(0.0)),
        _ => value.clone(),
    }
}

/// `serde_json` cannot hold infinity or NaN, so a non-finite float becomes `0`: one bad
/// number must not sink the whole submit.
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
            // The range is part of the contract, so it is required: a random parameter
            // without one never reaches a run.
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
        ParamType::Boolean => Err(ParamError::TypeMismatch {
            param: name.to_string(),
            expected: "integer / float / string",
            got: param.ty.as_str(),
        }),
    }
}

/// The bytes come from `/dev/urandom`; if it cannot be opened the clock is used instead,
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
mod tests {
    use super::*;
    use crate::config::manifest::Param;
    use serde_json::json;

    fn param(target: &[&str], ty: ParamType) -> Param {
        Param {
            target: target.iter().map(|s| s.to_string()).collect(),
            ty,
            default: None,
            random: false,
            range: None,
            values: None,
            description: None,
        }
    }

    fn manifest(params: &[(&str, Param)]) -> Manifest {
        Manifest {
            description: "demo".to_string(),
            params: params
                .iter()
                .map(|(name, param)| (name.to_string(), param.clone()))
                .collect(),
        }
    }

    fn json() -> Json {
        json!({
            "3": {"inputs": {"seed": 1, "steps": 20, "cfg": 8.0}},
            "4": {"inputs": {"model": ["1", 0]}},
            "6": {"inputs": {"text": "old"}},
            "18:7": {"inputs": {"steps": 25, "cfg": 2.5}},
        })
    }

    fn counter() -> impl FnMut() -> u64 {
        let mut n = 0;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn writes_deep_paths_and_subgraph_ids() {
        let m = manifest(&[
            ("prompt", param(&["6", "inputs", "text"], ParamType::String)),
            (
                "steps",
                param(&["18:7", "inputs", "steps"], ParamType::Integer),
            ),
        ]);
        let params = json!({"prompt": "a cat", "steps": 30});
        let mut rng = counter();
        let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
        assert_eq!(out["6"]["inputs"]["text"], json!("a cat"));
        assert_eq!(out["18:7"]["inputs"]["steps"], json!(30));
    }

    #[test]
    fn unknown_param_lists_all_allowed_names_sorted() {
        let m = manifest(&[
            ("beta", param(&["6", "inputs", "text"], ParamType::String)),
            (
                "alpha",
                param(&["3", "inputs", "steps"], ParamType::Integer),
            ),
        ]);
        let params = json!({"zeta": 1, "gamma": 2});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("an unknown parameter must be rejected");
        assert_eq!(
            err,
            ParamError::Unknown {
                unknown: vec!["gamma".to_string(), "zeta".to_string()],
                allowed: vec!["alpha".to_string(), "beta".to_string()],
            }
        );
    }

    #[test]
    fn missing_required_lists_every_item_with_description() {
        let mut a = param(&["6", "inputs", "text"], ParamType::String);
        a.description = Some("positive prompt".to_string());
        let b = param(&["3", "inputs", "steps"], ParamType::Integer);
        let m = manifest(&[("prompt", a), ("steps", b)]);
        let mut rng = counter();
        let err = resolve(&m, &json(), None, &mut rng)
            .expect_err("a missing required parameter must be rejected");
        assert_eq!(
            err,
            ParamError::Missing {
                items: vec![
                    ("prompt".to_string(), Some("positive prompt".to_string())),
                    ("steps".to_string(), None),
                ],
            }
        );
    }

    #[test]
    fn default_is_written_when_omitted() {
        let mut p = param(&["3", "inputs", "steps"], ParamType::Integer);
        p.default = Some(json!(28));
        let m = manifest(&[("steps", p)]);
        let template = json();
        let mut rng = counter();
        let out = resolve(&m, &template, None, &mut rng).unwrap();
        assert_eq!(out["3"]["inputs"]["steps"], json!(28));
        assert_eq!(
            template["3"]["inputs"]["steps"],
            json!(20),
            "the workflow JSON must not be mutated"
        );
    }

    #[test]
    fn omitted_without_default_is_rejected() {
        let m = manifest(&[(
            "steps",
            param(&["3", "inputs", "steps"], ParamType::Integer),
        )]);
        let mut rng = counter();
        let err = resolve(&m, &json(), None, &mut rng)
            .expect_err("a parameter without default must be supplied");
        assert!(matches!(err, ParamError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn float_accepts_integer_and_writes_float() {
        let m = manifest(&[("cfg", param(&["3", "inputs", "cfg"], ParamType::Float))]);
        let params = json!({"cfg": 8});
        let mut rng = counter();
        let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
        assert_eq!(out["3"]["inputs"]["cfg"], json!(8.0));
        assert!(out["3"]["inputs"]["cfg"].is_f64());
    }

    #[test]
    fn integer_rejects_float_and_string() {
        let m = manifest(&[(
            "steps",
            param(&["3", "inputs", "steps"], ParamType::Integer),
        )]);

        let params = json!({"steps": 8.0});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("integer must not accept 8.0");
        assert_eq!(
            err,
            ParamError::TypeMismatch {
                param: "steps".to_string(),
                expected: "integer",
                got: "float",
            }
        );

        let params = json!({"steps": "8"});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("integer must not accept a string");
        assert_eq!(
            err,
            ParamError::TypeMismatch {
                param: "steps".to_string(),
                expected: "integer",
                got: "string",
            }
        );
    }

    #[test]
    fn boolean_rejects_number() {
        let m = manifest(&[("flag", param(&["3", "inputs", "flag"], ParamType::Boolean))]);
        let params = json!({"flag": 1});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("boolean must not accept a number");
        assert_eq!(
            err,
            ParamError::TypeMismatch {
                param: "flag".to_string(),
                expected: "boolean",
                got: "integer",
            }
        );
    }

    #[test]
    fn explicit_zero_beats_random() {
        let mut p = param(&["3", "inputs", "seed"], ParamType::Integer);
        p.random = true;
        p.range = Some((0.0, 100.0));
        let m = manifest(&[("seed", p)]);
        let params = json!({"seed": 0});
        let mut rng = counter();
        let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
        assert_eq!(out["3"]["inputs"]["seed"], json!(0));
    }

    #[test]
    fn random_integer_stays_in_range_and_differs_across_calls() {
        let mut p = param(&["3", "inputs", "seed"], ParamType::Integer);
        p.random = true;
        p.range = Some((10.0, 20.0));
        let m = manifest(&[("seed", p)]);
        let mut rng = counter();

        let first = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["seed"]
            .as_i64()
            .expect("integer");
        let second = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["seed"]
            .as_i64()
            .expect("integer");
        assert!((10..=20).contains(&first), "{first}");
        assert!((10..=20).contains(&second), "{second}");
        assert_ne!(first, second, "two draws must differ");
    }

    #[test]
    fn random_float_uses_range() {
        let mut p = param(&["3", "inputs", "cfg"], ParamType::Float);
        p.random = true;
        p.range = Some((1.0, 30.0));
        let m = manifest(&[("cfg", p)]);
        let mut rng = counter();
        for _ in 0..5 {
            let value = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["cfg"]
                .as_f64()
                .expect("float");
            assert!((1.0..=30.0).contains(&value), "{value}");
        }
    }

    #[test]
    fn explicit_value_outside_range_is_rejected() {
        let mut p = param(&["3", "inputs", "steps"], ParamType::Integer);
        p.range = Some((1.0, 100.0));
        let m = manifest(&[("steps", p)]);
        let params = json!({"steps": 200});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("out-of-range must be rejected");
        assert_eq!(
            err,
            ParamError::OutOfRange {
                param: "steps".to_string(),
                min: 1.0,
                max: 100.0,
                got: 200.0,
            }
        );
    }

    #[test]
    fn explicit_value_outside_enum_is_rejected() {
        let mut p = param(&["6", "inputs", "text"], ParamType::String);
        p.values = Some(vec!["a".to_string(), "b".to_string()]);
        let m = manifest(&[("prompt", p)]);
        let params = json!({"prompt": "c"});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("a value outside enum must be rejected");
        assert_eq!(
            err,
            ParamError::NotInEnum {
                param: "prompt".to_string(),
                allowed: vec!["a".to_string(), "b".to_string()],
                got: "c".to_string(),
            }
        );
    }

    #[test]
    fn writing_to_connection_or_missing_path_is_bad_target() {
        let m = manifest(&[("model", param(&["4", "inputs", "model"], ParamType::String))]);
        let params = json!({"model": "x"});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("a link is not a parameter");
        assert_eq!(
            err,
            ParamError::BadTarget {
                param: "model".to_string(),
                pointer: "/4/inputs/model".to_string(),
            }
        );

        let m = manifest(&[("x", param(&["9", "inputs", "x"], ParamType::Integer))]);
        let params = json!({"x": 1});
        let mut rng = counter();
        let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
            .expect_err("the path does not exist");
        assert_eq!(
            err,
            ParamError::BadTarget {
                param: "x".to_string(),
                pointer: "/9/inputs/x".to_string(),
            }
        );
    }

    #[test]
    fn pointer_segments_are_escaped() {
        let p = param(&["a/b", "c~d"], ParamType::String);
        assert_eq!(p.pointer(), "/a~1b/c~0d");
    }
}
