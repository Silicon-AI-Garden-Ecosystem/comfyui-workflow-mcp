//! Defaults for `run`, kept out of `config.toml`:
//! that file is a read-only build output in most deployments,
//! while this one changes at runtime.
//! A broken file only fails a call that needs it.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultArgs {
    #[serde(default)]
    pub host_name: Option<String>,
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

pub fn load(path: &Path) -> Result<DefaultArgs, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(DefaultArgs::default()),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    toml::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, text: &str) -> std::path::PathBuf {
        let path = dir.join("default_args.toml");
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_missing_file_means_no_defaults() {
        let dir = TempDir::new().unwrap();
        let loaded = load(&dir.path().join("default_args.toml")).unwrap();
        assert_eq!(loaded, DefaultArgs::default());
    }

    #[test]
    fn every_key_is_optional() {
        let dir = TempDir::new().unwrap();
        let path = write(dir.path(), "timeout_seconds = 60\n");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.timeout_seconds, Some(60));
        assert_eq!(loaded.host_name, None);
    }

    #[test]
    fn all_keys_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = write(
            dir.path(),
            "host_name = \"a\"\nworkflow_name = \"b\"\noutput_path = \"~/out\"\ntimeout_seconds = 7\n",
        );
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.host_name.as_deref(), Some("a"));
        assert_eq!(loaded.workflow_name.as_deref(), Some("b"));
        assert_eq!(loaded.output_path.as_deref(), Some("~/out"));
        assert_eq!(loaded.timeout_seconds, Some(7));
    }

    #[test]
    fn an_unknown_key_names_the_file() {
        let dir = TempDir::new().unwrap();
        let path = write(dir.path(), "hostname = \"a\"\n");
        let err = load(&path).unwrap_err();
        assert!(err.starts_with(&path.display().to_string()), "{err}");
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn a_negative_timeout_is_a_parse_error() {
        let dir = TempDir::new().unwrap();
        let path = write(dir.path(), "timeout_seconds = -1\n");
        assert!(load(&path).is_err());
    }
}
