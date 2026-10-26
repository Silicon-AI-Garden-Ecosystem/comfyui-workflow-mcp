//! stdout carries the protocol and nothing else: a stray `println!` does not fail loudly, it
//! just corrupts the stream the client parses, so the contract is asserted against the real
//! binary instead of against a mocked transport.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value as Json;
use tempfile::TempDir;

struct Session {
    responses: Vec<Json>,
    stderr: String,
}

fn workflows_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("sim/workflows")
}

/// The config path is fixed, so `XDG_CONFIG_HOME` is the only lever that isolates a test
/// from the user's real configuration.
fn xdg_config_home(dir: &Path) -> PathBuf {
    dir.join("xdg")
}

fn write_config(dir: &Path) -> PathBuf {
    write_config_extra(dir, "")
}

/// The same config plus extra text in `[descriptions]`; the host list stays in `env.toml`.
fn write_config_extra(dir: &Path, extra: &str) -> PathBuf {
    write_config_for(dir, &workflows_dir(), extra)
}

fn write_config_for(dir: &Path, workflows: &Path, extra: &str) -> PathBuf {
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

fn session(requests: &[&str]) -> Session {
    let dir = TempDir::new().unwrap();
    write_config(dir.path());
    session_in(dir.path(), requests)
}

fn session_in(dir: &Path, requests: &[&str]) -> Session {
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

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"stdio-test","version":"0"}}}"#;
const LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

fn call(id: u32, name: &str, arguments: Json) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
    .to_string()
}

fn ids(session: &Session) -> Vec<u64> {
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

fn text_of(response: &Json) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {response}"))
}

fn body_of(session: &Session, id: u64) -> Json {
    let response = session
        .responses
        .iter()
        .find(|response| response["id"] == id)
        .expect("response for the requested id");
    serde_json::from_str(text_of(response)).unwrap()
}

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
}

#[test]
fn tools_list_exposes_both_tools() {
    let search = call(3, "search", serde_json::json!({}));
    let session = session(&[INIT, LIST, &search]);

    let list = session
        .responses
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tools/list response");

    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["run", "search"]);

    let info = session.responses.iter().find(|r| r["id"] == 1).unwrap();
    assert_eq!(info["result"]["serverInfo"]["name"], "comfyui-workflow-mcp");
}

#[test]
fn run_exposes_an_optional_output_path() {
    let list_only = session(&[INIT, LIST]);
    let list = list_only.responses.iter().find(|r| r["id"] == 2).unwrap();
    let run = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "run")
        .unwrap();

    let output_path = &run["inputSchema"]["properties"]["output_path"];
    assert_eq!(output_path["type"], serde_json::json!(["string", "null"]));
    assert!(
        output_path["description"]
            .as_str()
            .unwrap()
            .contains("root directory")
    );
    let required: Vec<&str> = run["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert!(required.contains(&"host_name"), "{required:?}");
    assert!(required.contains(&"workflow_name"), "{required:?}");
    assert!(!required.contains(&"output_path"), "{required:?}");

    // The rejection happens during validation, so the test never needs a live ComfyUI.
    let run_call = call(
        3,
        "run",
        serde_json::json!({
            "host_name": "local",
            "workflow_name": "image_sdxl_simple",
            "output_path": "relative/out",
        }),
    );
    let session = session(&[INIT, &run_call]);
    let response = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("must be absolute"),
        "{}",
        text_of(response)
    );
}

#[test]
fn search_without_arguments_lists_hosts() {
    let search = call(3, "search", serde_json::json!({}));
    let session = session(&[INIT, LIST, &search]);

    let body = body_of(&session, 3);
    let hosts = body["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1, "{body}");
    assert_eq!(hosts[0]["name"], "local");
    assert_eq!(hosts[0]["description"], "test host");
    // The address is not part of the answer: a host may point at something private.
    assert!(hosts[0].get("base_url").is_none(), "{body}");
    assert!(
        body["hint"].as_str().unwrap().contains("host_name"),
        "{body}"
    );
}

#[test]
fn search_with_a_host_lists_its_workflows() {
    let search = call(3, "search", serde_json::json!({ "host_name": "local" }));
    let session = session(&[INIT, LIST, &search]);

    let body = body_of(&session, 3);
    assert_eq!(body["host"], "local");
    let items = body["workflows"].as_array().unwrap();
    assert!(!items.is_empty(), "{body}");

    // The names come from the files, so the list follows their order and nothing else.
    let names: Vec<&str> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(body["hint"].as_str().unwrap().contains("workflow_name"));
}

#[test]
fn search_with_a_workflow_expands_params() {
    let search = call(
        3,
        "search",
        serde_json::json!({ "host_name": "local", "workflow_name": "image_sdxl_simple" }),
    );
    let session = session(&[INIT, LIST, &search]);

    let body = body_of(&session, 3);
    assert_eq!(body["host"], "local");
    assert_eq!(body["name"], "image_sdxl_simple");
    assert_eq!(body["params"]["prompt"]["required"], true);
    // A parameter with a default carries it; the value itself belongs to the workflow,
    // not to the test.
    assert!(body["params"]["width"].get("default").is_some(), "{body}");
    // `random` looks like `optional` to the model; the server draws the value for this run.
    assert!(body["params"]["seed"].get("random").is_none());
    assert_eq!(body["params"]["seed"]["optional"], true);
}

#[test]
fn a_workflow_name_without_a_host_is_rejected() {
    let search = call(
        3,
        "search",
        serde_json::json!({ "workflow_name": "image_sdxl_simple" }),
    );
    let session = session(&[INIT, &search]);

    let response = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("needs host_name"),
        "{}",
        text_of(response)
    );
}

#[test]
fn file_parameters_are_shown_as_file_type() {
    let search = call(
        3,
        "search",
        serde_json::json!({
            "host_name": "local",
            "workflow_name": "image_qwen_image_2_1_image_edit",
        }),
    );
    let session = session(&[INIT, LIST, &search]);

    let body = body_of(&session, 3);
    let params = body["params"].as_object().unwrap();
    let files: Vec<_> = params
        .iter()
        .filter(|(_, param)| param["type"] == "file")
        .collect();

    assert!(!files.is_empty(), "{body}");
    for (name, param) in &files {
        assert_eq!(param["type"], "file", "{name}: {body}");
    }
    // A hook makes a parameter optional without giving it a default value: the workflow
    // drops the input instead of filling it in.
    assert_eq!(params["image_1"]["required"], true, "{body}");
    assert_eq!(params["image_2"]["optional"], true, "{body}");
    assert!(params["image_2"].get("default").is_none(), "{body}");
}

#[test]
fn unknown_names_list_every_alternative() {
    let hosts = call(3, "search", serde_json::json!({}));
    let workflows = call(4, "search", serde_json::json!({ "host_name": "local" }));
    let bad_host = call(5, "search", serde_json::json!({ "host_name": "nope" }));
    let bad_workflow = call(
        6,
        "search",
        serde_json::json!({ "host_name": "local", "workflow_name": "image_sdxl_simpl" }),
    );
    let session = session(&[INIT, &hosts, &workflows, &bad_host, &bad_workflow]);

    let listed = body_of(&session, 4);
    let available: Vec<&str> = listed["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert!(!available.is_empty());

    let response = session.responses.iter().find(|r| r["id"] == 5).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("unknown host \"nope\""),
        "{}",
        text_of(response)
    );

    let response = session.responses.iter().find(|r| r["id"] == 6).unwrap();
    assert_eq!(response["result"]["isError"], true);
    let text = text_of(response);
    assert!(
        text.contains("unknown workflow \"image_sdxl_simpl\""),
        "{text}"
    );
    for name in available {
        assert!(text.contains(name), "{text} must list {name}");
    }
}

#[test]
fn runs_without_config_file_using_defaults() {
    let dir = TempDir::new().unwrap();
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
    // The default workflows directory does not exist yet, but the built-in host list does:
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
    let dir = TempDir::new().unwrap();
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

#[test]
fn tool_text_comes_from_the_config() {
    let dir = TempDir::new().unwrap();
    write_config_extra(dir.path(), "search_tool = \"CUSTOM SEARCH\"\n");

    let search = call(
        3,
        "search",
        serde_json::json!({ "host_name": "local", "workflow_name": "image_sdxl_simple" }),
    );
    let session = session_in(dir.path(), &[INIT, LIST, &search]);

    let list = session.responses.iter().find(|r| r["id"] == 2).unwrap();
    let tool = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "search")
        .unwrap();
    assert_eq!(tool["description"], "CUSTOM SEARCH");

    // Keys that were not overridden keep the built-in text.
    let run = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "run")
        .unwrap();
    assert!(
        run["description"]
            .as_str()
            .unwrap()
            .contains("absolute path")
    );

    let detail = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    let detail: Json = serde_json::from_str(text_of(detail)).unwrap();
    assert_eq!(detail["params"]["prompt"]["required"], true);
}
