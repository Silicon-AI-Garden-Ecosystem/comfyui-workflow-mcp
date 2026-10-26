//! The configuration is the settings file, the address file and the workflow set: the first
//! two are files the user owns, the third is a directory that may still be empty. A broken
//! workflow never fails the configuration -- it stays out of the set and the reason is logged.

mod env;
pub(crate) mod manifest;
mod setting;
mod workflow;

pub use env::{Env, Host};
pub use manifest::{Manifest, Param, ParamType};
pub use setting::{Descriptions, LogLevel, Setting};
pub use workflow::{Workflow, Workflows};

#[derive(Debug)]
pub struct Config {
    pub setting: Setting,
    pub env: Env,
    pub workflows: Workflows,
}

pub fn load() -> Result<Config, String> {
    combine(setting::load()?)
}

fn combine(setting: Setting) -> Result<Config, String> {
    let env = env::load(&setting.paths.env_file)?;
    let workflows = workflow::load(&setting.paths.workflows_dir, &env.hosts);
    Ok(Config {
        setting,
        env,
        workflows,
    })
}

/// Host names become directory names, workflow names come from file names: both must be a
/// single path component, and enforcing it here keeps "look up by name" from ever becoming
/// "build a path from a name".
pub(crate) fn validate_name(kind: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{kind} name is empty"));
    }
    if name == "." || name == ".." {
        return Err(format!("{kind} name must not be \".\" or \"..\""));
    }
    if name.contains('/') {
        return Err(format!("{kind} name must not contain \"/\""));
    }
    if name.contains('\0') {
        return Err(format!("{kind} name must not contain NUL"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{kind} name must not contain control characters"));
    }
    if name.trim() != name {
        return Err(format!(
            "{kind} name must not have leading or trailing whitespace"
        ));
    }
    if name.len() > 255 {
        return Err(format!("{kind} name exceeds 255 bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn no_vars(_name: &str) -> Option<String> {
        None
    }

    fn write_config(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("config.toml");
        fs::write(&path, text).unwrap();
        path
    }

    fn load_from(config_file: &Path, home: &Path) -> Result<Config, String> {
        combine(setting::load_with(config_file, home, &no_vars)?)
    }

    /// Points the config at files inside the temp dir, so the real environment stays untouched.
    fn config_text(dir: &Path, env_file: &Path) -> String {
        format!(
            "[paths]\nenv_file = \"{}\"\nworkflows_dir = \"{}\"\n",
            env_file.display(),
            dir.join("workflows").display()
        )
    }

    #[test]
    fn env_is_read_from_the_path_the_config_declares() {
        let dir = TempDir::new().unwrap();
        let env_file = dir.path().join("custom.toml");
        fs::write(
            &env_file,
            "[[hosts]]\nname = \"local\"\ndescription = \"d\"\nbase_url = \"http://from-the-config:1234\"\n",
        )
        .unwrap();
        let config_file = write_config(dir.path(), &config_text(dir.path(), &env_file));

        let loaded = load_from(&config_file, dir.path()).unwrap();

        assert_eq!(loaded.env.hosts[0].base_url, "http://from-the-config:1234");
    }

    #[test]
    fn a_broken_config_is_reported_before_the_env_is_read() {
        let dir = TempDir::new().unwrap();
        let config_file = write_config(dir.path(), "log_level = \n");

        let error = load_from(&config_file, dir.path()).unwrap_err();

        assert!(error.contains("config.toml"), "{error}");
        assert!(!error.contains(".env"), "{error}");
    }
}
