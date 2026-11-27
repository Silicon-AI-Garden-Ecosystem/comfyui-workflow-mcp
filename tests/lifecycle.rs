//! The binary is the only place where stdout is observable:
//! a stray `println!` corrupts the stream instead of failing loudly,
//! so the protocol and the reload path are asserted against the real process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::Value as Json;

mod common;

use common::*;

#[test]
fn stdout_carries_only_jsonrpc() {
    let search = call(3, "search", serde_json::json!({}));
    let session = session(&[INIT, LIST, &search]);

    assert_eq!(
        session.responses.len(),
        3,
        "stdout must hold exactly 3 lines: {:?}\nstderr: {}",
        session.responses,
        session.stderr
    );
    assert_eq!(ids(&session), [1, 2, 3]);

    let info = session.responses.iter().find(|r| r["id"] == 1).unwrap();
    assert_eq!(info["result"]["serverInfo"]["name"], "comfyui-workflow-mcp");

    let names: Vec<&str> =
        session.responses.iter().find(|r| r["id"] == 2).unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
    assert_eq!(names, ["run", "search"]);
}

#[test]
fn runs_without_config_file_using_defaults() {
    let dir = tempfile::TempDir::new().unwrap();
    let search = call(3, "search", serde_json::json!({}));
    let session = session_in(dir.path(), &[INIT, LIST, &search]);

    assert_eq!(ids(&session), [1, 2, 3]);
    // The startup log names the config it read, so a wrong file is visible at a glance.
    assert!(
        session.stderr.contains("config"),
        "the startup log must name the config file path: {}",
        session.stderr
    );
    assert!(
        session.stderr.contains("comfyui-workflow-mcp/config.toml"),
        "the startup log must name the config file path: {}",
        session.stderr
    );
    // The default workflows directory does not exist yet,
    // but the built-in host list does:
    // the host is listed, it simply has no workflows.
    let body = body_of(&session, 3);
    assert!(!body["hosts"].as_array().unwrap().is_empty(), "{body}");

    // Falling back to the defaults does not mean creating them on the way.
    assert!(
        !xdg_config_home(dir.path())
            .join("comfyui-workflow-mcp/config.toml")
            .exists()
    );
}

#[test]
fn a_new_workflow_shows_up_without_restarting() {
    // The workflows are re-read on every call: an agent may never restart the server.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("workflows");
    let host_dir = root.join("local");
    std::fs::create_dir_all(&host_dir).unwrap();
    write_config_for(dir.path(), &root, "");

    let mut child = Command::new(env!("CARGO_BIN_EXE_comfyui-workflow-mcp"))
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", xdg_config_home(dir.path()))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary should start");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut exchange = |request: &str| -> Json {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("the response is JSON")
    };

    exchange(INIT);
    let before = exchange(&call(
        2,
        "search",
        serde_json::json!({ "host_name": "local" }),
    ));
    let before: Json = serde_json::from_str(text_of(&before)).unwrap();
    assert_eq!(before["workflows"].as_array().unwrap().len(), 0);

    // A workflow arrives as a file pair, so both halves have to appear.
    for extension in ["json", "toml"] {
        std::fs::copy(
            workflows_dir()
                .join("local")
                .join(format!("image_sdxl_simple.{extension}")),
            host_dir.join(format!("image_sdxl_simple.{extension}")),
        )
        .unwrap();
    }

    let after = exchange(&call(
        3,
        "search",
        serde_json::json!({ "host_name": "local" }),
    ));
    let listed: Json = serde_json::from_str(text_of(&after)).unwrap();
    let names: Vec<&str> = listed["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["image_sdxl_simple"]);

    drop(stdin);
    child.wait().unwrap();
}
