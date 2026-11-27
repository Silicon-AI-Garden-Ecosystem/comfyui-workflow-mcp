use std::fs;

use tempfile::TempDir;

use super::*;
use crate::config::validate_name;

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

    // The default config points at a directory that does not exist yet: 0 workflows, not a failure.
    let missing = load(&root.path().join("nope"), &hosts(&["local"]));
    assert!(missing.is_empty());
    assert!(missing.broken().is_empty());
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
    fs::write(dir.join("README.md"), "hi").unwrap();
    fs::write(dir.join("preview.png"), b"png").unwrap();

    let workflows = load(root.path(), &hosts(&["local"]));

    assert_eq!(workflows.names_of("local"), ["visible"]);
    assert!(workflows.broken().is_empty(), "{:?}", workflows.broken());
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
