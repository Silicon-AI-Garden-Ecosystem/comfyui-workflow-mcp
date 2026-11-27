//! The workflow set is one directory per host, rescanned on every call:
//! a server may live for a whole session, so a workflow added minutes ago must work without a restart.
//! A name is a file name, and a directory whose name is not a host in `env.toml` is reported, not ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

use crate::core::error::LoadError;

use super::Host;
use super::manifest::Manifest;

/// A workflow is `<name>.json` plus `<name>.toml`:
/// the JSON stays as ComfyUI exports it, the TOML is the parameter contract.
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

    /// Exact lookup on both names:
    /// the model repeats what it was shown,
    /// so a fuzzy match would quietly run a different workflow,
    /// or the same workflow on the wrong host.
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

mod scan;

use scan::{load_pair, pairs, scan_root};

#[cfg(test)]
mod tests;
