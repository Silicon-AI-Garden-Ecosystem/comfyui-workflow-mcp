//! The workflow set is the content of one directory per host, rescanned on every tool call:
//! an agent may hold a server for a whole session, so a workflow added minutes ago must work
//! without a restart. A name is a file name: what the file manager shows is what the model
//! passes. Hosts come from `env.toml`, so a directory whose name is not a host is reported
//! instead of being silently ignored.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

use crate::core::error::LoadError;

use super::Host;
use super::manifest::Manifest;
use super::validate_name;

/// A workflow is `<name>.json` plus `<name>.toml`: the JSON stays as ComfyUI exports it,
/// the TOML is the parameter contract.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub host: String,
    pub name: String,
    pub description: String,
    pub manifest: Manifest,
    pub json: Json,
    /// The path seen while scanning. A name is a lookup key only, never part of a path.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Workflows {
    hosts: Vec<Host>,
    workflows: Vec<Workflow>,
    by_key: BTreeMap<(String, String), usize>,
    broken: Vec<LoadError>,
}

impl Workflows {
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    pub fn host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.name == name)
    }

    pub fn of_host(&self, host: &str) -> Vec<&Workflow> {
        self.workflows
            .iter()
            .filter(|workflow| workflow.host == host)
            .collect()
    }

    /// Exact lookup on both names: the model repeats what it was shown, so a fuzzy match would
    /// quietly run a different workflow -- or the same workflow on the wrong host.
    pub fn get(&self, host: &str, name: &str) -> Option<&Workflow> {
        self.by_key
            .get(&(host.to_string(), name.to_string()))
            .map(|index| &self.workflows[*index])
    }

    pub fn names_of(&self, host: &str) -> Vec<String> {
        self.of_host(host)
            .into_iter()
            .map(|workflow| workflow.name.clone())
            .collect()
    }

    pub fn all(&self) -> &[Workflow] {
        &self.workflows
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

pub fn load(dir: &Path, hosts: &[Host]) -> Workflows {
    let mut workflows = Workflows {
        hosts: hosts.to_vec(),
        ..Workflows::default()
    };

    if let Err(error) = scan_root(dir, hosts, &mut workflows.broken) {
        workflows.broken.push(error);
        return workflows;
    }

    let order: BTreeMap<&str, usize> = hosts
        .iter()
        .enumerate()
        .map(|(index, host)| (host.name.as_str(), index))
        .collect();

    for host in hosts {
        let (jsons, errors) = pairs(&dir.join(&host.name));
        workflows.broken.extend(errors);
        for json in jsons {
            match load_pair(&json, &host.name) {
                Ok(workflow) => workflows.workflows.push(workflow),
                Err(mut errs) => workflows.broken.append(&mut errs),
            }
        }
    }

    workflows.workflows.sort_by(|a, b| {
        order
            .get(a.host.as_str())
            .cmp(&order.get(b.host.as_str()))
            .then_with(|| a.name.as_bytes().cmp(b.name.as_bytes()))
    });
    for (index, workflow) in workflows.workflows.iter().enumerate() {
        workflows
            .by_key
            .insert((workflow.host.clone(), workflow.name.clone()), index);
    }
    workflows
}

/// Workflows live under `<workflows_dir>/<host>/`, so anything else at the top level is a
/// layout mistake worth reporting: a flat `workflows/foo.json` would otherwise look like
/// "my workflow disappeared" with no hint why.
fn scan_root(dir: &Path, hosts: &[Host], broken: &mut Vec<LoadError>) -> Result<(), LoadError> {
    let at = LoadError::at_file(dir);
    let metadata = match fs::metadata(dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(LoadError::new(
                &at,
                format!("failed to read the workflows directory: {err}"),
            ));
        }
    };
    if !metadata.is_dir() {
        return Err(LoadError::new(&at, "workflows root is not a directory"));
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            return Err(LoadError::new(
                &at,
                format!("failed to read the workflows directory: {err}"),
            ));
        }
    };

    let known: std::collections::BTreeSet<&str> =
        hosts.iter().map(|host| host.name.as_str()).collect();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                broken.push(LoadError::new(
                    &at,
                    format!("failed to read a directory entry: {err}"),
                ));
                continue;
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        match fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {
                if !known.contains(name.as_ref()) {
                    broken.push(LoadError::new(
                        LoadError::at_file(&path),
                        format!("no host named {name:?} in env.toml: workflows live in <host>/"),
                    ));
                }
            }
            Ok(meta) if meta.is_file() => {
                if matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("json" | "toml")
                ) {
                    broken.push(LoadError::new(
                        LoadError::at_file(&path),
                        "workflow files must live in <workflows_dir>/<host>/",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A missing directory is an empty set, not a failure: a host may not have any workflow yet.
/// An unreadable path is a broken entry, and so is a half pair.
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
            vec![LoadError::new(&at, "host directory is not a directory")],
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
fn load_pair(json_path: &Path, host: &str) -> Result<Workflow, Vec<LoadError>> {
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
            if let Err(message) = validate_name("workflow", stem) {
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
        host: host.to_string(),
        name,
        description: manifest.description.clone(),
        manifest,
        json,
        path: json_path.to_path_buf(),
    })
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

    fn hosts(names: &[&str]) -> Vec<Host> {
        names
            .iter()
            .map(|name| Host {
                name: name.to_string(),
                description: format!("{name} endpoint"),
                base_url: "http://127.0.0.1:8188".to_string(),
            })
            .collect()
    }

    fn host_dir(root: &Path, host: &str) -> PathBuf {
        let dir = root.join(host);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make(root: &Path, name: &str) -> PathBuf {
        make_in(&host_dir(root, "local"), name)
    }

    fn make_in(dir: &Path, name: &str) -> PathBuf {
        let json = dir.join(format!("{name}.json"));
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(dir.join(format!("{name}.toml")), toml_for()).unwrap();
        json
    }

    #[test]
    fn loads_a_valid_pair() {
        let root = TempDir::new().unwrap();
        let json = make(root.path(), "sdxl_portrait");

        let workflows = load(root.path(), &hosts(&["local"]));

        let workflow = workflows.get("local", "sdxl_portrait").unwrap();
        assert_eq!(workflow.host, "local");
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
        let dir = host_dir(root.path(), "local");
        make_in(&dir, "good");
        fs::write(dir.join("bad.json"), "{\n  \"a\": ,\n}").unwrap();
        fs::write(dir.join("bad.toml"), toml_for()).unwrap();

        let workflows = load(root.path(), &hosts(&["local"]));

        assert_eq!(workflows.names_of("local"), ["good"]);
        assert_eq!(workflows.broken().len(), 1, "{:?}", workflows.broken());
        assert!(
            workflows.broken()[0].message.contains("JSON"),
            "{:?}",
            workflows.broken()
        );

        // Every workflow broken is not a failure either: there is simply nothing to find.
        let all_broken = TempDir::new().unwrap();
        let dir = host_dir(all_broken.path(), "local");
        fs::write(dir.join("bad.json"), b"not json").unwrap();
        fs::write(dir.join("bad.toml"), b"also not toml").unwrap();
        let workflows = load(all_broken.path(), &hosts(&["local"]));
        assert!(workflows.is_empty());
        assert!(!workflows.broken().is_empty());
    }

    #[test]
    fn missing_toml_is_reported() {
        let root = TempDir::new().unwrap();
        let dir = host_dir(root.path(), "local");
        let json = dir.join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();

        let workflows = load(root.path(), &hosts(&["local"]));

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
        let dir = host_dir(root.path(), "local");
        fs::write(dir.join("README.md"), "hi").unwrap();
        fs::write(dir.join("preview.png"), b"png").unwrap();

        assert_eq!(
            load(root.path(), &hosts(&["local"])).names_of("local"),
            ["wf"]
        );
    }

    #[test]
    fn validate_name_rejects_slash_control_and_padding() {
        for bad in ["a/b", "a\nb", "a\rb", " a", "a ", "", ".", ".."] {
            assert!(validate_name("workflow", bad).is_err(), "{bad:?}");
        }
        // Allowed: a name may be anything a file manager can show, because the contract is the file name.
        for ok in ["naïve name", "a b", "🎨", "UPPER", "-dash"] {
            assert!(validate_name("workflow", ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn broken_json_has_line_number() {
        let root = TempDir::new().unwrap();
        let dir = host_dir(root.path(), "local");
        let json = dir.join("wf.json");
        fs::write(&json, "{\n  \"a\": ,\n}").unwrap();
        fs::write(dir.join("wf.toml"), toml_for()).unwrap();

        let errors = load_pair(&json, "local").unwrap_err();
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
        let dir = host_dir(root.path(), "local");
        let json = dir.join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(dir.join("wf.toml"), "[params.x]\ntarget = [\n").unwrap();

        let errors = load_pair(&json, "local").unwrap_err();
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
        let dir = host_dir(root.path(), "local");
        let json = dir.join("wf.json");
        fs::write(&json, TEMPLATE).unwrap();
        fs::write(
            dir.join("wf.toml"),
            r#"
description = "d"

[params.prompt]
target = ["6", "inputs", "does_not_exist"]
type = "string"
"#,
        )
        .unwrap();

        let errors = load_pair(&json, "local").unwrap_err();
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

        let workflows = load(root.path(), &hosts(&["local"]));

        assert_eq!(workflows.len(), 4);
        let names = workflows.names_of("local");
        assert_eq!(names, ["Beta", "alpha", "zeta", "Ünicode"]);
        assert!(workflows.get("local", "alpha").is_some());
        assert!(workflows.get("local", "nope").is_none());
        assert!(!workflows.is_empty());
    }

    #[test]
    fn the_same_name_can_exist_on_two_hosts() {
        let root = TempDir::new().unwrap();
        make_in(&host_dir(root.path(), "local"), "wf");
        make_in(&host_dir(root.path(), "gpu"), "wf");

        let workflows = load(root.path(), &hosts(&["local", "gpu"]));

        assert_eq!(workflows.len(), 2);
        assert_eq!(workflows.get("local", "wf").unwrap().host, "local");
        assert_eq!(workflows.get("gpu", "wf").unwrap().host, "gpu");
        assert_eq!(workflows.names_of("local"), ["wf"]);
    }

    #[test]
    fn a_host_without_a_directory_has_no_workflows() {
        let root = TempDir::new().unwrap();

        let workflows = load(root.path(), &hosts(&["local"]));

        assert!(workflows.is_empty());
        assert!(workflows.broken().is_empty());
        assert!(workflows.host("local").is_some());
    }

    #[test]
    fn a_directory_that_is_not_a_host_is_reported() {
        let root = TempDir::new().unwrap();
        make(root.path(), "wf");
        host_dir(root.path(), "stray");

        let workflows = load(root.path(), &hosts(&["local"]));

        assert_eq!(workflows.names_of("local"), ["wf"]);
        assert_eq!(workflows.broken().len(), 1, "{:?}", workflows.broken());
        assert!(
            workflows.broken()[0]
                .message
                .contains("no host named \"stray\""),
            "{:?}",
            workflows.broken()
        );
    }

    #[test]
    fn a_flat_pair_at_the_root_is_reported() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("flat.json"), TEMPLATE).unwrap();
        fs::write(root.path().join("flat.toml"), toml_for()).unwrap();

        let workflows = load(root.path(), &hosts(&["local"]));

        assert!(workflows.is_empty());
        assert!(
            workflows
                .broken()
                .iter()
                .any(|e| e.message.contains("<host>")),
            "{:?}",
            workflows.broken()
        );
    }

    #[test]
    fn unpaired_files_are_reported() {
        let root = TempDir::new().unwrap();
        let dir = host_dir(root.path(), "local");
        fs::write(dir.join("one.json"), TEMPLATE).unwrap();
        fs::write(dir.join("two.toml"), toml_for()).unwrap();
        make_in(&dir, "good");

        let workflows = load(root.path(), &hosts(&["local"]));

        assert_eq!(workflows.names_of("local"), ["good"]);
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
        let dir = host_dir(root.path(), "local");
        make_in(&dir, "visible");
        fs::write(dir.join(".hidden.json"), b"{}").unwrap();
        fs::write(dir.join(".DS_Store"), b"junk").unwrap();
        fs::write(root.path().join(".hidden.json"), b"{}").unwrap();

        let workflows = load(root.path(), &hosts(&["local"]));

        assert_eq!(workflows.names_of("local"), ["visible"]);
        assert!(workflows.broken().is_empty(), "{:?}", workflows.broken());
    }

    #[test]
    fn missing_root_is_empty_not_error() {
        let root = TempDir::new().unwrap();
        // The default config points at a directory that does not exist yet: 0 workflows, not a failure.
        let workflows = load(&root.path().join("nope"), &hosts(&["local"]));

        assert!(workflows.is_empty());
        assert!(workflows.broken().is_empty());
    }

    #[test]
    fn root_must_be_directory() {
        let root = TempDir::new().unwrap();
        let file = root.path().join("file");
        fs::write(&file, b"x").unwrap();

        let workflows = load(&file, &hosts(&["local"]));

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
        make_in(outside.path(), "linked");
        let dir = host_dir(root.path(), "local");
        for ext in ["json", "toml"] {
            std::os::unix::fs::symlink(
                outside.path().join(format!("linked.{ext}")),
                dir.join(format!("linked.{ext}")),
            )
            .unwrap();
        }

        assert_eq!(
            load(root.path(), &hosts(&["local"])).names_of("local"),
            ["linked"]
        );
    }
}
