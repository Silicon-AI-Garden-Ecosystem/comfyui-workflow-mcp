use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

const BUILTIN_TOML: &str = include_str!("../../sim/config.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    pub env_file: PathBuf,
    pub logs_dir: PathBuf,
    pub output_dir: PathBuf,
    pub workflows_dir: PathBuf,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timings {
    pub timeout_seconds: u64,
    pub poll_interval_ms: u64,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptions {
    pub search_tool: String,
    pub run_tool: String,
    pub host_name_arg: String,
    pub workflow_name_arg: String,
    pub output_path_arg: String,
    pub hint_hosts: String,
    pub hint_workflows: String,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub log_level: LogLevel,
    pub paths: Paths,
    pub timings: Timings,
    pub descriptions: Descriptions,
}

pub fn load() -> Result<Setting, String> {
    let home = home_dir()?;
    let config_file = user_config_path(&home);
    load_with(&config_file, &home, &|name| env::var(name).ok())
}

pub(crate) fn load_with(
    config_file: &Path,
    home: &Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<Setting, String> {
    let builtin = expand_paths(
        parse_toml(BUILTIN_TOML, "sim/config.toml")?,
        home,
        home,
        vars,
    )?;
    build(builtin.clone(), "sim/config.toml")?;

    let user = match fs::read_to_string(config_file) {
        Ok(text) => {
            let base = config_file.parent().unwrap_or(home);
            expand_paths(
                parse_toml(&text, &config_file.display().to_string())?,
                base,
                home,
                vars,
            )?
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Value::Table(Default::default()),
        Err(err) => return Err(format!("{}: {err}", config_file.display())),
    };

    build(merge(user, builtin), &config_file.display().to_string())
}

fn user_config_path(home: &Path) -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".config"));
    base.join("comfyui-workflow-mcp/config.toml")
}

fn parse_toml(text: &str, file: &str) -> Result<Value, String> {
    toml::from_str(text).map_err(|err| format!("{file}: {err}"))
}

fn build(value: Value, file: &str) -> Result<Setting, String> {
    value.try_into().map_err(|err| format!("{file}: {err}"))
}

fn merge(user: Value, builtin: Value) -> Value {
    match (user, builtin) {
        (Value::Table(mut user), Value::Table(builtin)) => {
            for (key, value) in builtin {
                let merged = match user.remove(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                user.insert(key, merged);
            }
            Value::Table(user)
        }
        (user, _) => user,
    }
}

fn expand_paths(
    value: Value,
    base: &Path,
    home: &Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<Value, String> {
    let mut table = match value {
        Value::Table(table) => table,
        _ => return Err("expected a table".to_string()),
    };
    if let Some(paths) = table.get_mut("paths").and_then(Value::as_table_mut) {
        for (key, value) in paths.iter_mut() {
            if let Some(text) = value.as_str() {
                let expanded = expand_path(text, base, home, vars)
                    .map_err(|err| format!("paths.{key}: {err}"))?;
                *value = Value::String(expanded.to_string_lossy().into_owned());
            }
        }
    }
    Ok(Value::Table(table))
}

fn expand_path(
    raw: &str,
    base: &Path,
    home: &Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    let text = expand_vars(raw, vars)?;
    let text = match text.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => text,
    };
    let path = PathBuf::from(text);
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}

fn expand_vars(raw: &str, vars: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unterminated ${{ in {raw:?}"))?;
        let expr = &after[..end];
        let (name, fallback) = match expr.split_once(":-") {
            Some((name, fallback)) => (name, Some(fallback)),
            None => (expr, None),
        };
        match vars(name).or_else(|| fallback.map(str::to_string)) {
            Some(value) => out.push_str(&value),
            None => return Err(format!("undefined variable {name} in {raw:?}")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| "HOME is not set".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_config(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("comfyui-workflow-mcp").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn no_vars(_name: &str) -> Option<String> {
        None
    }

    fn builtin_raw() -> Value {
        toml::from_str(BUILTIN_TOML).unwrap()
    }

    fn builtin_setting(home: &Path) -> Setting {
        load_with(&home.join("missing.toml"), home, &no_vars).unwrap()
    }

    #[test]
    fn builtin_paths_are_absolute_and_fully_expanded() {
        let home = TempDir::new().unwrap();
        let config = builtin_setting(home.path());

        for path in [
            &config.paths.env_file,
            &config.paths.logs_dir,
            &config.paths.output_dir,
            &config.paths.workflows_dir,
        ] {
            assert!(path.is_absolute(), "{path:?} is not absolute");
            assert!(
                !path.to_string_lossy().contains("${"),
                "{path:?} still has a placeholder"
            );
        }
    }

    #[test]
    fn builtin_paths_follow_the_variables_the_file_names() {
        let home = TempDir::new().unwrap();
        let data = home.path().join("data");
        let cache = home.path().join("cache");
        let state = home.path().join("state");
        let vars = |name: &str| match name {
            "XDG_DATA_HOME" => Some(data.display().to_string()),
            "XDG_CACHE_HOME" => Some(cache.display().to_string()),
            "XDG_STATE_HOME" => Some(state.display().to_string()),
            _ => None,
        };
        let config = load_with(&home.path().join("missing.toml"), home.path(), &vars).unwrap();
        let raw = builtin_raw();

        let pairs = [
            (
                "workflows_dir",
                &config.paths.workflows_dir,
                "XDG_DATA_HOME",
                &data,
            ),
            (
                "output_dir",
                &config.paths.output_dir,
                "XDG_CACHE_HOME",
                &cache,
            ),
            ("logs_dir", &config.paths.logs_dir, "XDG_STATE_HOME", &state),
        ];
        for (key, path, variable, expected) in pairs {
            let template = raw["paths"][key].as_str().unwrap();
            if template.contains(variable) {
                assert!(
                    path.starts_with(expected),
                    "{key} should start with {variable}"
                );
            }
        }
    }

    #[test]
    fn missing_user_file_gives_the_builtin() {
        let home = TempDir::new().unwrap();
        let empty = write_config(home.path(), "");
        let missing = home
            .path()
            .join("comfyui-workflow-mcp")
            .join("nothing.toml");

        let from_missing = load_with(&missing, home.path(), &no_vars).unwrap();
        let from_empty = load_with(&empty, home.path(), &no_vars).unwrap();

        assert_eq!(from_missing, from_empty);
    }

    #[test]
    fn user_file_overrides_only_what_it_sets() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "log_level = \"debug\"\n");
        let config = load_with(&file, home.path(), &no_vars).unwrap();
        let builtin = builtin_setting(home.path());

        assert_eq!(config.log_level, LogLevel::Debug);
        assert_eq!(config.paths, builtin.paths);
        assert_eq!(config.timings, builtin.timings);
        assert_eq!(config.descriptions, builtin.descriptions);
    }

    #[test]
    fn relative_path_resolves_against_the_declaring_file() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "[paths]\noutput_dir = \"out\"\n");
        let config = load_with(&file, home.path(), &no_vars).unwrap();

        assert_eq!(config.paths.output_dir, file.parent().unwrap().join("out"));
    }

    #[test]
    fn tilde_resolves_to_home() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "[paths]\noutput_dir = \"~/pics\"\n");
        let config = load_with(&file, home.path(), &no_vars).unwrap();

        assert_eq!(config.paths.output_dir, home.path().join("pics"));
    }

    #[test]
    fn variable_is_expanded() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "[paths]\noutput_dir = \"${A_VAR}/out\"\n");
        let vars = |name: &str| match name {
            "A_VAR" => Some("/tmp/a-var".to_string()),
            _ => None,
        };
        let config = load_with(&file, home.path(), &vars).unwrap();

        assert_eq!(config.paths.output_dir, PathBuf::from("/tmp/a-var/out"));
    }

    #[test]
    fn undefined_variable_is_rejected() {
        let home = TempDir::new().unwrap();
        let file = write_config(
            home.path(),
            "[paths]\noutput_dir = \"${NO_SUCH_VAR}/out\"\n",
        );
        let error = load_with(&file, home.path(), &no_vars).unwrap_err();

        assert!(error.contains("NO_SUCH_VAR"), "{error}");
    }

    #[test]
    fn variable_default_is_used_when_unset() {
        let home = TempDir::new().unwrap();
        let file = write_config(
            home.path(),
            "[paths]\noutput_dir = \"${NO_SUCH_VAR:-fallback}\"\n",
        );
        let config = load_with(&file, home.path(), &no_vars).unwrap();

        assert_eq!(
            config.paths.output_dir,
            file.parent().unwrap().join("fallback")
        );
    }

    #[test]
    fn variable_wins_over_its_default() {
        let home = TempDir::new().unwrap();
        let file = write_config(
            home.path(),
            "[paths]\noutput_dir = \"${A_VAR:-fallback}\"\n",
        );
        let vars = |name: &str| match name {
            "A_VAR" => Some("/tmp/a-var".to_string()),
            _ => None,
        };
        let config = load_with(&file, home.path(), &vars).unwrap();

        assert_eq!(config.paths.output_dir, PathBuf::from("/tmp/a-var"));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "nope = 1\n");
        let error = load_with(&file, home.path(), &no_vars).unwrap_err();

        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn invalid_log_level_is_rejected() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "log_level = \"verbose\"\n");
        let error = load_with(&file, home.path(), &no_vars).unwrap_err();

        assert!(error.contains("verbose"), "{error}");
        assert!(error.contains("unknown variant"), "{error}");
    }

    #[test]
    fn broken_toml_is_rejected() {
        let home = TempDir::new().unwrap();
        let file = write_config(home.path(), "log_level = \n");
        let error = load_with(&file, home.path(), &no_vars).unwrap_err();

        assert!(error.contains(&file.display().to_string()), "{error}");
    }
}
