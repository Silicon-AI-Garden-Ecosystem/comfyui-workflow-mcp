use std::path::Path;

use serde_json::Value as Json;

use super::*;
use crate::core::error::LoadError;

impl Manifest {
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

        for (name, param) in &self.params {
            for hook in &param.hooks {
                let paths = hook.remove.iter().chain(hook.set.iter().map(|s| &s.target));
                for path in paths {
                    let pointer = pointer(path);
                    if json.pointer(&pointer).is_none() {
                        errors.push(LoadError::new(
                            at.clone(),
                            format!(
                                "params.{name}.hook target={pointer} does not exist in the workflow JSON"
                            ),
                        ));
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::manifest::test_support::*;

    #[test]
    fn parses_and_validates_sample_workflows() {
        let samples = [
            (
                include_str!("../../../sim/workflows/local/image_sdxl_simple.toml"),
                include_str!("../../../sim/workflows/local/image_sdxl_simple.json"),
            ),
            (
                include_str!("../../../sim/workflows/local/image_qwen_image_2_1_t2i.toml"),
                include_str!("../../../sim/workflows/local/image_qwen_image_2_1_t2i.json"),
            ),
            (
                include_str!("../../../sim/workflows/local/image_qwen_image_2_1_image_edit.toml"),
                include_str!("../../../sim/workflows/local/image_qwen_image_2_1_image_edit.json"),
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
        let sdxl = parse(include_str!(
            "../../../sim/workflows/local/image_sdxl_simple.toml"
        ))
        .unwrap();
        assert!(sdxl.params["prompt"].required());
        assert!(!sdxl.params["negative_prompt"].required());
        assert!(sdxl.params["seed"].random);
        assert_eq!(sdxl.params["seed"].default, None);
        assert!(sdxl.params["width"].default.is_some());

        // Image parameters are handed to the upload path instead of the JSON, so their
        // target must be the string field that ComfyUI reads the name from.
        let edit = parse(include_str!(
            "../../../sim/workflows/local/image_qwen_image_2_1_image_edit.toml"
        ))
        .unwrap();
        let edit_json: Json = serde_json::from_str(include_str!(
            "../../../sim/workflows/local/image_qwen_image_2_1_image_edit.json"
        ))
        .unwrap();
        let images: Vec<_> = edit
            .params
            .iter()
            .filter(|(_, param)| param.ty == ParamType::File)
            .collect();
        assert!(!images.is_empty());
        for (name, param) in images {
            let value = edit_json
                .pointer(&param.pointer())
                .unwrap_or_else(|| panic!("{name} must point at a field of the sample"));
            assert!(value.is_string(), "{name} must point at a string field");
            assert!(!param.random, "{name} cannot be drawn");
        }
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

    #[test]
    fn a_hook_path_must_exist_in_the_json() {
        let m = parse(&param_toml(
            "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\", \"nope\"]]\n",
        ))
        .unwrap();
        let errors = m.validate(&validate_json(), path()).unwrap_err();
        assert!(
            errors[0].message.contains("hook target=/3/inputs/nope"),
            "{}",
            errors[0].message
        );
    }
}
