use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde::Deserialize;

use super::validate_name;

/// Endpoints are a list, not a set of keys: the file is a contract with a shape, and an
/// array keeps the order the user wrote them in.
const BUILTIN_ENV: &str = include_str!("../../sim/env.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub description: String,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    pub hosts: Vec<Host>,
}

pub fn load(env_file: &Path) -> Result<Env, String> {
    match fs::read_to_string(env_file) {
        Ok(text) => parse(&text, &env_file.display().to_string()),
        // A missing file is the normal first run: the built-in list points at localhost.
        Err(err) if err.kind() == ErrorKind::NotFound => {
            parse(BUILTIN_ENV, "sim/env.toml (built in)")
        }
        Err(err) => Err(format!("{}: {err}", env_file.display())),
    }
}

/// The user file replaces the built-in host list instead of merging into it: a list has an
/// order and a length, and half of it coming from elsewhere makes "delete a host" mean
/// nothing. Missing entries are a parse error, so the shape stays one file.
fn parse(text: &str, at: &str) -> Result<Env, String> {
    let env: Env = toml::from_str(text).map_err(|err| format!("{at}: {err}"))?;
    let mut seen = BTreeSet::new();
    for host in &env.hosts {
        validate_name("host", &host.name).map_err(|message| format!("{at}: {message}"))?;
        if !seen.insert(host.name.clone()) {
            return Err(format!("{at}: duplicate host {:?}", host.name));
        }
        if host.base_url.trim().is_empty() {
            return Err(format!("{at}: host {:?} has an empty base_url", host.name));
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_env(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("env.toml");
        fs::write(&path, text).unwrap();
        path
    }

    fn builtin_env() -> Env {
        parse(BUILTIN_ENV, "sim/env.toml").unwrap()
    }

    #[test]
    fn the_builtin_lists_at_least_one_host_with_a_name_and_description() {
        let env = builtin_env();

        assert!(!env.hosts.is_empty());
        for host in &env.hosts {
            assert!(!host.name.is_empty());
            assert!(!host.description.is_empty());
            assert!(host.base_url.starts_with("http"));
        }
    }

    #[test]
    fn a_missing_file_gives_the_builtin() {
        let dir = TempDir::new().unwrap();

        let loaded = load(&dir.path().join("missing.toml")).unwrap();

        assert_eq!(loaded, builtin_env());
    }

    #[test]
    fn the_user_file_replaces_the_builtin_list() {
        let dir = TempDir::new().unwrap();
        let file = write_env(
            dir.path(),
            "[[hosts]]\nname = \"gpu\"\ndescription = \"remote\"\nbase_url = \"http://gpu:8188\"\n",
        );

        let loaded = load(&file).unwrap();

        assert_eq!(
            loaded.hosts,
            vec![Host {
                name: "gpu".to_string(),
                description: "remote".to_string(),
                base_url: "http://gpu:8188".to_string(),
            }]
        );
        assert_ne!(loaded.hosts, builtin_env().hosts);
    }

    #[test]
    fn duplicate_host_names_are_rejected() {
        let dir = TempDir::new().unwrap();
        let file = write_env(
            dir.path(),
            "[[hosts]]\nname = \"a\"\ndescription = \"x\"\nbase_url = \"http://a\"\n\
             [[hosts]]\nname = \"a\"\ndescription = \"y\"\nbase_url = \"http://b\"\n",
        );

        let error = load(&file).unwrap_err();

        assert!(error.contains("duplicate host"), "{error}");
    }

    #[test]
    fn a_host_name_must_be_one_path_component() {
        let dir = TempDir::new().unwrap();
        let file = write_env(
            dir.path(),
            "[[hosts]]\nname = \"a/b\"\ndescription = \"x\"\nbase_url = \"http://a\"\n",
        );

        let error = load(&file).unwrap_err();

        assert!(error.contains("host name must not contain"), "{error}");
    }

    #[test]
    fn an_empty_base_url_is_rejected() {
        let dir = TempDir::new().unwrap();
        let file = write_env(
            dir.path(),
            "[[hosts]]\nname = \"a\"\ndescription = \"x\"\nbase_url = \"  \"\n",
        );

        let error = load(&file).unwrap_err();

        assert!(error.contains("empty base_url"), "{error}");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = TempDir::new().unwrap();
        let file = write_env(
            dir.path(),
            "[[hosts]]\nname = \"a\"\ndescription = \"x\"\nbase_url = \"http://a\"\ntoken = \"t\"\n",
        );

        let error = load(&file).unwrap_err();

        assert!(error.contains("token"), "{error}");
    }
}
