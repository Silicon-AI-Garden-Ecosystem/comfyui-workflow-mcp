use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde::Deserialize;
use serde::de::value::MapDeserializer;

const BUILTIN_ENV: &str = include_str!("../../sim/.env");

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    #[serde(rename = "BASE_URL")]
    pub base_url: String,
}

fn builtin() -> Result<HashMap<String, String>, String> {
    parse_values(BUILTIN_ENV, "sim/.env")
}

pub fn load(env_file: &Path) -> Result<Env, String> {
    let builtin = builtin()?;
    to_env(builtin.clone(), "sim/.env")?;

    let user = match fs::read_to_string(env_file) {
        Ok(text) => parse_values(&text, &env_file.display().to_string())?,
        Err(err) if err.kind() == ErrorKind::NotFound => HashMap::new(),
        Err(err) => return Err(format!("{}: {err}", env_file.display())),
    };

    to_env(merge(builtin, user), &env_file.display().to_string())
}

fn merge(
    builtin: HashMap<String, String>,
    user: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = builtin;
    merged.extend(user);
    merged
}

fn to_env(values: HashMap<String, String>, file: &str) -> Result<Env, String> {
    let deserializer: MapDeserializer<_, serde::de::value::Error> =
        MapDeserializer::new(values.into_iter());
    Env::deserialize(deserializer).map_err(|err| format!("{file}: {err}"))
}

fn parse_values(text: &str, file: &str) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("{file}: line {} is not KEY=VALUE: {line}", index + 1))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{file}: line {} has no variable name", index + 1));
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(value);
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_env(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    fn builtin_values() -> HashMap<String, String> {
        builtin().unwrap()
    }

    fn builtin_env() -> Env {
        to_env(builtin_values(), "sim/.env").unwrap()
    }

    fn first_key(values: &HashMap<String, String>) -> (String, String) {
        let mut keys: Vec<&String> = values.keys().collect();
        keys.sort();
        let key = keys[0].clone();
        let value = values[&key].clone();
        (key, value)
    }

    fn render(values: &HashMap<String, String>) -> String {
        let mut lines: Vec<String> = values
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        lines.sort();
        lines.join("\n") + "\n"
    }

    fn user_variant(values: &HashMap<String, String>) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| {
                let mutated = format!("{value}-user");
                let mut candidate = values.clone();
                candidate.insert(key.clone(), mutated.clone());
                let usable = to_env(candidate, "sim/.env").is_ok();
                (key.clone(), if usable { mutated } else { value.clone() })
            })
            .collect()
    }

    #[test]
    fn parse_ignores_comments_blank_lines_and_export() {
        let (key, value) = first_key(&builtin_values());
        let text = format!("# comment\n\nexport {key}={value}\n");
        let values = parse_values(&text, "fixture").unwrap();

        assert_eq!(values, HashMap::from([(key, value)]));
    }

    #[test]
    fn parse_strips_quotes_and_trims_values() {
        let (key, value) = first_key(&builtin_values());
        let text = format!("{key} = \"{value}\" \n");
        let values = parse_values(&text, "fixture").unwrap();

        assert_eq!(values, HashMap::from([(key, value)]));
    }

    #[test]
    fn parse_rejects_a_line_without_equals() {
        let error = parse_values("oops\n", "fixture").unwrap_err();

        assert!(error.contains("line 1"), "{error}");
    }

    #[test]
    fn parse_rejects_an_empty_key() {
        let error = parse_values("=value\n", "fixture").unwrap_err();

        assert!(error.contains("line 1"), "{error}");
    }

    #[test]
    fn merge_lets_the_user_win() {
        let builtin = builtin_values();
        let user: HashMap<String, String> = builtin
            .iter()
            .map(|(key, value)| (key.clone(), format!("{value}-user")))
            .collect();
        let merged = merge(builtin, user.clone());

        assert_eq!(merged, user);
    }

    #[test]
    fn merge_falls_back_for_missing_keys() {
        let builtin = builtin_values();
        let mut user: HashMap<String, String> = builtin
            .iter()
            .map(|(key, value)| (key.clone(), format!("{value}-user")))
            .collect();
        let mut expected = user.clone();
        let dropped = first_key(&builtin).0;
        user.remove(&dropped);
        expected.insert(dropped.clone(), builtin[&dropped].clone());
        let merged = merge(builtin, user);

        assert_eq!(merged, expected);
    }

    #[test]
    fn missing_file_gives_the_builtin() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.env");

        assert_eq!(load(&missing).unwrap(), builtin_env());
    }

    #[test]
    fn a_file_without_usable_keys_gives_the_builtin() {
        let dir = TempDir::new().unwrap();

        let empty = write_env(dir.path(), "empty.env", "");
        assert_eq!(load(&empty).unwrap(), builtin_env());

        let comments = write_env(dir.path(), "comments.env", "# only a comment\n\n");
        assert_eq!(load(&comments).unwrap(), builtin_env());
    }

    #[test]
    fn user_values_win() {
        let dir = TempDir::new().unwrap();
        let builtin = builtin_values();
        let user_values = user_variant(&builtin);
        let file = write_env(dir.path(), "user.env", &render(&user_values));
        let expected = to_env(user_values.clone(), "sim/.env").unwrap();
        let loaded = load(&file).unwrap();

        assert_eq!(loaded, expected);
        if user_values != builtin {
            assert_ne!(loaded, builtin_env());
        }
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut user_values = user_variant(&builtin_values());
        user_values.insert("UNLISTED_KEY".to_string(), "value".to_string());
        let file = write_env(dir.path(), "unknown.env", &render(&user_values));
        let error = load(&file).unwrap_err();

        assert!(error.contains("UNLISTED_KEY"), "{error}");
    }

    #[test]
    fn unreadable_path_is_rejected() {
        let dir = TempDir::new().unwrap();
        let error = load(dir.path()).unwrap_err();

        assert!(error.contains(&dir.path().display().to_string()), "{error}");
    }
}
