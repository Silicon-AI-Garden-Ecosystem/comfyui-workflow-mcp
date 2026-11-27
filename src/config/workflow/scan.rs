use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

use crate::config::Host;
use crate::config::manifest::Manifest;
use crate::config::validate_name;
use crate::core::error::LoadError;

use super::Workflow;

// Workflows live under `<workflows_dir>/<host>/`,
// so anything else at the top level is a layout mistake worth reporting:
// a flat `workflows/foo.json` would otherwise look like "my workflow disappeared" with no hint why.
pub(super) fn scan_root(
    dir: &Path,
    hosts: &[Host],
    broken: &mut Vec<LoadError>,
) -> Result<(), LoadError> {
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

// A missing directory is an empty set, not a failure:
// a host may not have any workflow yet.
// An unreadable path is a broken entry, and so is a half pair.
pub(super) fn pairs(dir: &Path) -> (Vec<PathBuf>, Vec<LoadError>) {
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
        // `metadata` follows symlinks: a link to a real file counts,
        // a broken one is skipped, without content there is no pair,
        // and the file name stays the identity.
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

// All errors are collected, so one broken pair reports everything wrong with it at once.
pub(super) fn load_pair(json_path: &Path, host: &str) -> Result<Workflow, Vec<LoadError>> {
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
