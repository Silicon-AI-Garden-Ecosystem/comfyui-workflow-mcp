//! The workflow set is the content of one directory, rescanned on every tool call: an agent
//! may hold a server for a whole session, so a workflow added minutes ago must work without a
//! restart. A name is a file name: what the file manager shows is what the model passes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

use crate::core::error::LoadError;

use super::manifest::Manifest;

/// A workflow is `<name>.json` plus `<name>.toml`: the JSON stays as ComfyUI exports it,
/// the TOML is the parameter contract.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub manifest: Manifest,
    pub json: Json,
    /// The path seen while scanning. A name is a lookup key only, never part of a path.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Workflows {
    workflows: Vec<Workflow>,
    by_name: BTreeMap<String, usize>,
    broken: Vec<LoadError>,
}

impl Workflows {
    pub fn all(&self) -> &[Workflow] {
        &self.workflows
    }

    /// Exact name lookup: the model repeats the name it was shown, so a fuzzy match would
    /// quietly run a different workflow.
    pub fn get(&self, name: &str) -> Option<&Workflow> {
        self.by_name.get(name).map(|i| &self.workflows[*i])
    }

    pub fn names(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.workflows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.workflows.is_empty()
    }

    /// The pairs that failed to load: one author's typo must not take the server down.
    pub fn broken(&self) -> &[LoadError] {
        &self.broken
    }
}

pub fn load(dir: &Path) -> Workflows {
    let mut workflows = Workflows::default();

    let (jsons, errors) = pairs(dir);
    workflows.broken.extend(errors);
    for json in jsons {
        match load_pair(&json) {
            Ok(workflow) => workflows.workflows.push(workflow),
            Err(mut errs) => workflows.broken.append(&mut errs),
        }
    }

    workflows
        .workflows
        .sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    for (index, workflow) in workflows.workflows.iter().enumerate() {
        workflows.by_name.insert(workflow.name.clone(), index);
    }
    workflows
}

/// A missing directory is an empty set, not a failure: the default points at a directory that
/// may not exist yet. An unreadable path is a broken entry, and so is a half pair.
fn pairs(dir: &Path) -> (Vec<PathBuf>, Vec<LoadError>) {
    let at = LoadError::at_file(dir);
    let metadata = match fs::metadata(dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(err) => {
            return (
                Vec::new(),
                vec![LoadError::new(
                    &at,
                    format!("failed to read the workflows directory: {err}"),
                )],
            );
        }
    };
    if !metadata.is_dir() {
        return (
            Vec::new(),
            vec![LoadError::new(&at, "workflows root is not a directory")],
        );
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                Vec::new(),
                vec![LoadError::new(
                    &at,
                    format!("failed to read the workflows directory: {err}"),
                )],
            );
        }
    };

    let mut jsons = Vec::new();
    let mut stems_with_json = BTreeMap::new();
    let mut tomls: Vec<PathBuf> = Vec::new();
    let mut errors = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                errors.push(LoadError::new(
                    &at,
                    format!("failed to read a directory entry: {err}"),
                ));
                continue;
            }
        };
        let path = entry.path();
        // Dotfiles are not part of the contract: the user sees a workflow as a visible file name.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // `metadata` follows symlinks: a link to a real file counts, a broken one is skipped --
        // without content there is no pair, and the file name stays the identity.
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) | Err(_) => continue,
        }
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    stems_with_json.insert(stem.to_string(), ());
                }
                jsons.push(path);
            }
            Some("toml") => tomls.push(path),
            _ => {}
        }
    }

    jsons.sort_by_key(|path| file_name(path));

    let mut paired = Vec::with_capacity(jsons.len());
    for json in jsons {
        let toml = json.with_extension("toml");
        if toml.is_file() {
            paired.push(json);
        } else {
            errors.push(LoadError::new(
                LoadError::at_file(&json),
                format!(
                    "missing {}: each .json needs a .toml with the same name",
                    toml.display()
                ),
            ));
        }
    }
    for toml in &tomls {
        let Some(stem) = toml.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stems_with_json.contains_key(stem) {
            errors.push(LoadError::new(
                LoadError::at_file(toml),
                format!(
                    "missing {}: each .toml needs a .json with the same name",
                    toml.with_extension("json").display()
                ),
            ));
        }
    }

    (paired, errors)
}

/// All errors are collected, so one broken pair reports everything wrong with it at once.
fn load_pair(json_path: &Path) -> Result<Workflow, Vec<LoadError>> {
    let at = LoadError::at_file(json_path);
    let toml_path = json_path.with_extension("toml");
    let mut errors = Vec::new();

    let name = match json_path.file_stem().and_then(|s| s.to_str()) {
        None => {
            errors.push(LoadError::new(
                &at,
                "cannot derive a workflow name from the file name",
            ));
            None
        }
        Some(stem) => {
            if let Err(message) = validate_name(stem) {
                errors.push(LoadError::new(&at, message));
            }
            Some(stem.to_string())
        }
    };

    let json = match fs::read_to_string(json_path) {
        Err(err) => {
            errors.push(LoadError::new(
                LoadError::at_file(json_path),
                format!("read failed: {err}"),
            ));
            None
        }
        Ok(text) => match serde_json::from_str::<Json>(&text) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                errors.push(LoadError::new(
                    LoadError::at_line(json_path, err.line()),
                    format!("JSON parse failed: {err}"),
                ));
                None
            }
        },
    };

    // The spans survive into the error, so a semantic mistake points at the line, not the file.
    let manifest = match fs::read_to_string(&toml_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            errors.push(LoadError::new(
                LoadError::at_file(&toml_path),
                format!(
                    "missing {}: each .json needs a .toml of the same name",
                    toml_path.display()
                ),
            ));
            None
        }
        Err(err) => {
            errors.push(LoadError::new(
                LoadError::at_file(&toml_path),
                format!("read failed: {err}"),
            ));
            None
        }
        Ok(text) => match Manifest::parse(&text, &toml_path) {
            Ok(manifest) => Some(manifest),
            Err(err) => {
                errors.push(err);
                None
            }
        },
    };

    if let (Some(manifest), Some(parsed)) = (manifest.as_ref(), json.as_ref())
        && let Err(mut errs) = manifest.validate(parsed, &toml_path)
    {
        errors.append(&mut errs);
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let manifest = manifest.expect("manifest is parsed when there are no errors");
    let json = json.expect("the JSON is parsed when there are no errors");
    let name = name.expect("the file name exists when there are no errors");
    Ok(Workflow {
        name,
        description: manifest.description.clone(),
        manifest,
        json,
        path: json_path.to_path_buf(),
    })
}

/// A name must be a single path component, even though it is never used to build one: the
/// path comes from the scan, so a hostile name can only miss, never escape the directory.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("workflow name is empty".to_string());
    }
    if name == "." || name == ".." {
        return Err("workflow name must not be \".\" or \"..\"".to_string());
    }
    if name.contains('/') {
        return Err("workflow name must not contain \"/\"".to_string());
    }
    if name.contains('\0') {
        return Err("workflow name must not contain NUL".to_string());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("workflow name must not contain control characters".to_string());
    }
    if name.trim() != name {
        return Err("workflow name must not have leading or trailing whitespace".to_string());
    }
    if name.len() > 255 {
        return Err("workflow name exceeds 255 bytes".to_string());
    }
    Ok(())
}

fn file_name(path: &Path) -> Vec<u8> {
    path.file_name()
        .map(|name| name.as_encoded_bytes().to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const TEMPLATE: &str = r#"{
  "3": { "class_type": "KSampler", "inputs": { "seed": 1, "steps": 20 } },
  "6": { "class_type": "CLIPTextEncode", "inputs": { "text": "a cat" } }
}"#;

    fn toml_for() -> &'static str {
        r#"
description = "test workflow"

[params.prompt]
target = ["6", "inputs", "text"]
type = "string"

[params.seed]
target = ["3", "inputs", "seed"]
type = "integer"
default = "random"
range = [0, 9007199254740991]
"#
    }

    fn make(root: &Path, name: &str) -> PathBuf {
        let json = root.join(format!("{name}.json"));
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(root.join(format!("{name}.toml")), toml_for()).unwrap();
        json
    }

    #[test]
    fn loads_a_valid_pair() {
        let root = TempDir::new().unwrap();
        let json = make(root.path(), "sdxl_portrait");
        let workflows = load(root.path());
        let workflow = workflows.get("sdxl_portrait").unwrap();
        assert_eq!(workflow.name, "sdxl_portrait");
        assert_eq!(workflow.description, "test workflow");
        assert_eq!(workflow.path, json);
        assert_eq!(workflow.manifest.params.len(), 2);
        assert!(workflow.manifest.params["seed"].random);
        assert_eq!(workflow.json["6"]["inputs"]["text"], Json::from("a cat"));
        assert!(workflows.broken().is_empty());
    }

    #[test]
    fn a_broken_pair_is_left_out_but_the_others_still_load() {
        let root = TempDir::new().unwrap();
        make(root.path(), "good");
        fs::write(root.path().join("bad.json"), "{\n  \"a\": ,\n}").unwrap();
        fs::write(root.path().join("bad.toml"), toml_for()).unwrap();

        let workflows = load(root.path());

        assert_eq!(workflows.names(), ["good"]);
        assert_eq!(workflows.broken().len(), 1, "{:?}", workflows.broken());
        assert!(
            workflows.broken()[0].message.contains("JSON"),
            "{:?}",
            workflows.broken()
        );

        // Every workflow broken is not a failure either: there is simply nothing to find.
        let all_broken = TempDir::new().unwrap();
        fs::write(all_broken.path().join("bad.json"), b"not json").unwrap();
        fs::write(all_broken.path().join("bad.toml"), b"also not toml").unwrap();
        let workflows = load(all_broken.path());
        assert!(workflows.is_empty());
        assert!(!workflows.broken().is_empty());
    }

    #[test]
    fn missing_toml_is_reported() {
        let root = TempDir::new().unwrap();
        let json = root.path().join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();

        let workflows = load(root.path());

        assert!(workflows.is_empty());
        assert_eq!(workflows.broken().len(), 1, "{:?}", workflows.broken());
        assert!(
            workflows.broken()[0].message.contains("wf.toml"),
            "{:?}",
            workflows.broken()
        );
    }

    #[test]
    fn other_files_next_to_the_pair_are_ignored() {
        let root = TempDir::new().unwrap();
        make(root.path(), "wf");
        fs::write(root.path().join("README.md"), "hi").unwrap();
        fs::write(root.path().join("preview.png"), b"png").unwrap();
        assert_eq!(load(root.path()).names(), ["wf"]);
    }

    #[test]
    fn validate_name_rejects_slash_control_and_padding() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_name("a\rb").is_err());
        assert!(validate_name(" a").is_err());
        assert!(validate_name("a ").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        // Allowed: a name may be anything a file manager can show, because the contract is the file name.
        for ok in ["naïve name", "a b", "🎨", "UPPER", "-dash"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn broken_json_has_line_number() {
        let root = TempDir::new().unwrap();
        let json = root.path().join("wf.json");
        fs::write(&json, "{\n  \"a\": ,\n}").unwrap();
        fs::write(root.path().join("wf.toml"), toml_for()).unwrap();

        let errors = load_pair(&json).unwrap_err();
        let err = errors
            .iter()
            .find(|e| e.message.contains("JSON"))
            .expect("expected a JSON error");
        assert!(err.at.contains("wf.json"), "{err:?}");
        assert!(err.at.contains(":2"), "{err:?}");
    }

    #[test]
    fn broken_toml_reports_line() {
        let root = TempDir::new().unwrap();
        let json = root.path().join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(root.path().join("wf.toml"), "[params.x]\ntarget = [\n").unwrap();

        let errors = load_pair(&json).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("TOML") || e.at.contains("wf.toml")),
            "{errors:?}"
        );
    }

    #[test]
    fn missing_target_reports_toml_line() {
        let root = TempDir::new().unwrap();
        let json = root.path().join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(
            root.path().join("wf.toml"),
            r#"
description = "d"

[params.prompt]
target = ["6", "inputs", "does_not_exist"]
type = "string"
"#,
        )
        .unwrap();

        let errors = load_pair(&json).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].at.contains("wf.toml"), "{errors:?}");
        assert!(
            errors[0].message.contains("does_not_exist"),
            "the error must name the offending target: {errors:?}"
        );
    }

    #[test]
    fn loads_and_sorts_by_name() {
        let root = TempDir::new().unwrap();
        for name in ["zeta", "alpha", "Ünicode", "Beta"] {
            make(root.path(), name);
        }

        let workflows = load(root.path());

        assert_eq!(workflows.len(), 4);
        let names: Vec<&str> = workflows.all().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Beta", "alpha", "zeta", "Ünicode"]);
        assert_eq!(workflows.names(), names);
        assert!(workflows.get("alpha").is_some());
        assert!(workflows.get("nope").is_none());
        assert!(!workflows.is_empty());
    }

    #[test]
    fn unpaired_files_are_reported() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("one.json"), TEMPLATE).unwrap();
        fs::write(root.path().join("two.toml"), toml_for()).unwrap();
        make(root.path(), "good");

        let workflows = load(root.path());

        assert_eq!(workflows.names(), ["good"]);
        assert_eq!(workflows.broken().len(), 2, "{:?}", workflows.broken());
        assert!(
            workflows
                .broken()
                .iter()
                .any(|e| e.at.ends_with("one.json") && e.message.contains("one.toml")),
            "{:?}",
            workflows.broken()
        );
        assert!(
            workflows
                .broken()
                .iter()
                .any(|e| e.at.ends_with("two.toml") && e.message.contains("two.json")),
            "{:?}",
            workflows.broken()
        );
    }

    #[test]
    fn hidden_files_are_skipped() {
        let root = TempDir::new().unwrap();
        make(root.path(), "visible");
        fs::write(root.path().join(".hidden.json"), b"{}").unwrap();
        fs::write(root.path().join(".DS_Store"), b"junk").unwrap();

        assert_eq!(load(root.path()).names(), ["visible"]);
    }

    #[test]
    fn missing_root_is_empty_not_error() {
        let root = TempDir::new().unwrap();
        // The default config points at a directory that does not exist yet: 0 workflows, not a failure.
        let workflows = load(&root.path().join("nope"));
        assert!(workflows.is_empty());
        assert!(workflows.broken().is_empty());
    }

    #[test]
    fn root_must_be_directory() {
        let root = TempDir::new().unwrap();
        let file = root.path().join("file");
        fs::write(&file, b"x").unwrap();

        let workflows = load(&file);

        assert!(workflows.is_empty());
        assert_eq!(workflows.broken().len(), 1, "{:?}", workflows.broken());
        assert!(
            workflows.broken()[0].message.contains("not a directory"),
            "{:?}",
            workflows.broken()
        );
    }

    #[test]
    fn symlinked_files_are_included() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        make(outside.path(), "linked");
        for ext in ["json", "toml"] {
            std::os::unix::fs::symlink(
                outside.path().join(format!("linked.{ext}")),
                root.path().join(format!("linked.{ext}")),
            )
            .unwrap();
        }

        assert_eq!(load(root.path()).names(), ["linked"]);
    }
}
