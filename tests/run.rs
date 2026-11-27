mod common;

use common::*;

#[test]
fn run_schema_and_early_rejections() {
    let list_only = session(&[INIT, LIST]);
    let run = tool(&list_only, "run");

    let output_path = &run["inputSchema"]["properties"]["output_path"];
    assert_eq!(output_path["type"], serde_json::json!(["string", "null"]));
    assert!(
        output_path["description"]
            .as_str()
            .unwrap()
            .contains("root directory")
    );

    let timeout = &run["inputSchema"]["properties"]["timeout_seconds"];
    assert!(timeout.is_object(), "{timeout}");
    assert!(
        timeout["description"]
            .as_str()
            .unwrap()
            .contains("greater than 0")
    );

    // A defaults file may supply any of them, so none can be marked required up front.
    let required = required_fields(run);
    for name in [
        "host_name",
        "workflow_name",
        "output_path",
        "timeout_seconds",
    ] {
        assert!(!required.contains(&name.to_string()), "{required:?}");
    }
    for name in ["host_name", "workflow_name"] {
        let description = run["inputSchema"]["properties"][name]["description"]
            .as_str()
            .unwrap();
        assert!(description.contains("default args file"), "{description}");
    }

    // Both rejections happen during validation, so no live ComfyUI is needed.
    let relative = call(
        3,
        "run",
        serde_json::json!({
            "host_name": "local",
            "workflow_name": "image_sdxl_simple",
            "output_path": "relative/out",
        }),
    );
    let zero = call(
        4,
        "run",
        serde_json::json!({
            "host_name": "local",
            "workflow_name": "image_sdxl_simple",
            "timeout_seconds": 0,
            "params": { "prompt": "x" },
        }),
    );
    let session = session(&[INIT, &relative, &zero]);

    let response = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("must be absolute"),
        "{}",
        text_of(response)
    );

    // Zero would mean "time out immediately", which is never what the caller wants.
    let response = session.responses.iter().find(|r| r["id"] == 4).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("timeout_seconds must be greater than 0"),
        "{}",
        text_of(response)
    );
}

#[test]
fn run_needs_host_and_workflow() {
    let without_file = session(&[INIT, &call(3, "run", serde_json::json!({ "params": {} }))]);
    let response = without_file
        .responses
        .iter()
        .find(|r| r["id"] == 3)
        .unwrap();
    assert_eq!(response["result"]["isError"], true);
    let text = text_of(response);
    assert!(text.contains("host_name is required"), "{text}");
    assert!(text.contains("default_args.toml"), "{text}");

    // A partial defaults file fills only what it names.
    let dir = tempfile::TempDir::new().unwrap();
    write_config(dir.path());
    write_defaults(dir.path(), "host_name = \"local\"\n");
    let partial = session_in(
        dir.path(),
        &[INIT, &call(3, "run", serde_json::json!({ "params": {} }))],
    );
    let response = partial.responses.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(
        text_of(response).contains("workflow_name is required"),
        "{}",
        text_of(response)
    );
}

#[test]
fn run_resolves_host_and_workflow_from_the_defaults_file() {
    let dir = tempfile::TempDir::new().unwrap();
    write_config(dir.path());
    let workflow = first_builtin_workflow();
    write_defaults(
        dir.path(),
        &format!("host_name = \"local\"\nworkflow_name = \"{workflow}\"\n"),
    );

    let run = call(3, "run", serde_json::json!({ "params": {} }));
    let session = session_in(dir.path(), &[INIT, &run]);
    let response = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    let text = text_of(response);

    assert_eq!(response["result"]["isError"], true);
    assert!(!text.contains("is required"), "{text}");
    assert!(
        session.stderr.contains("host_defaulted=true"),
        "{}",
        session.stderr
    );
    assert!(
        session.stderr.contains("workflow_defaulted=true"),
        "{}",
        session.stderr
    );
}

#[test]
fn a_broken_defaults_file_only_fails_calls_that_need_it() {
    let dir = tempfile::TempDir::new().unwrap();
    write_config(dir.path());
    write_defaults(dir.path(), "hostname = \"local\"\n");

    let needs_it = call(3, "run", serde_json::json!({ "params": {} }));
    let session = session_in(dir.path(), &[INIT, &needs_it]);
    let response = session.responses.iter().find(|r| r["id"] == 3).unwrap();

    assert_eq!(response["result"]["isError"], true);
    let text = text_of(response);
    assert!(text.contains("default_args.toml"), "{text}");
    assert!(text.contains("unknown field"), "{text}");
}

#[test]
fn error_text_comes_from_the_config() {
    let dir = tempfile::TempDir::new().unwrap();
    write_config(dir.path());
    append_to_config(
        dir.path(),
        "[messages.params]\nmissing = \"MISSING: {list}\"\n\n[messages.tools]\nworkflow_name_needs_host = \"NEED HOST\"\n",
    );

    let run = call(
        3,
        "run",
        serde_json::json!({ "host_name": "local", "workflow_name": "image_sdxl_simple", "params": {} }),
    );
    let search = call(
        4,
        "search",
        serde_json::json!({ "workflow_name": "image_sdxl_simple" }),
    );
    let session = session_in(dir.path(), &[INIT, LIST, &run, &search]);

    let run = session.responses.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(run["result"]["isError"], true);
    assert!(
        text_of(run).starts_with("MISSING: prompt"),
        "unexpected text: {}",
        text_of(run)
    );

    let search = session.responses.iter().find(|r| r["id"] == 4).unwrap();
    assert_eq!(text_of(search), "NEED HOST");
}
