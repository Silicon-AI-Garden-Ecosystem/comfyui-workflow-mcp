//! The configuration is the settings file, the address file and the workflow set: the first
//! two are files the user owns, the third is a directory that may still be empty. A broken
//! workflow never fails the configuration -- it stays out of the set and the reason is logged.

mod env;
pub(crate) mod manifest;
mod setting;
mod workflow;

pub use env::Env;
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
    let workflows = workflow::load(&setting.paths.workflows_dir);
    Ok(Config {
        setting,
        env,
        workflows,
    })
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
        let env_file = dir.path().join("custom.env");
        fs::write(&env_file, "BASE_URL=http://from-the-config:1234\n").unwrap();
        let config_file = write_config(dir.path(), &config_text(dir.path(), &env_file));

        let loaded = load_from(&config_file, dir.path()).unwrap();

        assert_eq!(loaded.env.base_url, "http://from-the-config:1234");
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
