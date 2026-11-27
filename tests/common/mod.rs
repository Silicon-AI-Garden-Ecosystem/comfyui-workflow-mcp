// Each test binary compiles its own copy of this module and uses a different subset,
// so an unused helper here is expected rather than dead code.
#![allow(dead_code)]

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value as Json;
use tempfile::TempDir;

pub struct Session {
    pub responses: Vec<Json>,
    pub stderr: String,
}

pub fn workflows_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("sim/workflows")
}

// The config path is fixed,
// so `XDG_CONFIG_HOME` is the only lever that isolates a test from the user's real configuration.
pub fn xdg_config_home(dir: &Path) -> PathBuf {
    dir.join("xdg")
}

pub fn write_config(dir: &Path) -> PathBuf {
    write_config_extra(dir, "")
}

// The same config plus extra text in `[descriptions]`; the host list stays in `env.toml`.
pub fn write_config_extra(dir: &Path, extra: &str) -> PathBuf {
    write_config_for(dir, &workflows_dir(), extra)
}

pub fn write_config_for(dir: &Path, workflows: &Path, extra: &str) -> PathBuf {
    let config_dir = xdg_config_home(dir).join("comfyui-workflow-mcp");
    std::fs::create_dir_all(&config_dir).unwrap();

    let env_file = dir.join("env.toml");
    std::fs::write(
        &env_file,
        "[[hosts]]\nname = \"local\"\ndescription = \"test host\"\nbase_url = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();

    let path = config_dir.join("config.toml");
    let body = format!(
        "log_level = \"info\"\n\n[paths]\nenv_file = \"{}\"\nlogs_dir = \"{}\"\noutput_dir = \"{}\"\nworkflows_dir = \"{}\"\n\n[timings]\ntimeout_seconds = 5\npoll_interval_ms = 20\n\n[descriptions]\n{}",
        env_file.display(),
        dir.join("logs").display(),
        dir.join("out").display(),
        workflows.display(),
        extra,
    );
    std::fs::write(&path, body).unwrap();
    path
}

// Appends a table after `[descriptions]`,
// a fresh header keeps the previous keys where they were,
// so a test can override one message without restating the rest.
pub fn append_to_config(dir: &Path, text: &str) {
    let path = xdg_config_home(dir)
        .join("comfyui-workflow-mcp")
        .join("config.toml");
    let mut body = std::fs::read_to_string(&path).unwrap();
    body.push_str("\n\n");
    body.push_str(text);
    std::fs::write(path, body).unwrap();
}

pub fn session(requests: &[&str]) -> Session {
    let dir = TempDir::new().unwrap();
    write_config(dir.path());
    session_in(dir.path(), requests)
}

pub fn session_in(dir: &Path, requests: &[&str]) -> Session {
    let mut child = Command::new(env!("CARGO_BIN_EXE_comfyui-workflow-mcp"))
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", xdg_config_home(dir))
        .env("XDG_STATE_HOME", dir.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary should start");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let lines: Vec<String> = requests.iter().map(|r| (*r).to_string()).collect();
    std::thread::spawn(move || {
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
    });

    // stderr is read on its own thread: a full pipe would block the child.
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf).unwrap();
        buf
    });

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        BufReader::new(stdout).read_to_string(&mut buf).unwrap();
        let _ = tx.send(buf);
    });

    let stdout_text = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            panic!("stdout was not closed within 30s, the server may be stuck");
        }
    };

    let status = child.wait().unwrap();
    let stderr = stderr_reader.join().unwrap();
    assert!(status.success(), "exit code {status}; stderr:\n{stderr}");

    let responses = stdout_text
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout contained a non-JSON line: {line:?} ({err})"))
        })
        .collect();

    Session { responses, stderr }
}

pub const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"stdio-test","version":"0"}}}"#;
pub const LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

pub fn call(id: u32, name: &str, arguments: Json) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
    .to_string()
}

pub fn ids(session: &Session) -> Vec<u64> {
    let mut ids: Vec<u64> = session
        .responses
        .iter()
        .map(|response| {
            response["id"]
                .as_u64()
                .expect("every response must carry an id")
        })
        .collect();
    ids.sort_unstable();
    ids
}

pub fn text_of(response: &Json) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {response}"))
}

// A schema with no required field omits the key entirely.
pub fn required_fields(tool: &Json) -> Vec<String> {
    tool["inputSchema"]["required"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn body_of(session: &Session, id: u64) -> Json {
    let response = session
        .responses
        .iter()
        .find(|response| response["id"] == id)
        .expect("response for the requested id");
    serde_json::from_str(text_of(response)).unwrap()
}

pub fn tool<'a>(session: &'a Session, name: &str) -> &'a Json {
    session
        .responses
        .iter()
        .find(|response| response["id"] == 2)
        .expect("tools/list response")["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("no tool named {name}"))
}

// The defaults path is derived from `XDG_STATE_HOME`, the only lever a test has.
pub fn defaults_path(dir: &Path) -> PathBuf {
    dir.join("state/comfyui-workflow-mcp/default_args.toml")
}

pub fn write_defaults(dir: &Path, text: &str) {
    let path = defaults_path(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// A copied name would go stale the day the fixture is renamed.
pub fn first_builtin_workflow() -> String {
    let mut names: Vec<String> = std::fs::read_dir(workflows_dir().join("local"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "json")
                .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names.remove(0)
}
