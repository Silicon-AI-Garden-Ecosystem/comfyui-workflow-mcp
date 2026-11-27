use serde_json::Value as Json;

mod common;

use common::*;

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
    assert!(body["hint"].as_str().unwrap().contains("workflow_name"));

    let names: Vec<&str> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"image_sdxl_simple"), "{body}");
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
    // A hook makes a parameter optional without giving it a default value:
    // the workflow drops the input instead of filling it in.
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
fn tool_text_comes_from_the_config() {
    let dir = tempfile::TempDir::new().unwrap();
    write_config_extra(dir.path(), "search_tool = \"CUSTOM SEARCH\"\n");

    let search = call(
        3,
        "search",
        serde_json::json!({ "host_name": "local", "workflow_name": "image_sdxl_simple" }),
    );
    let session = session_in(dir.path(), &[INIT, LIST, &search]);

    assert_eq!(tool(&session, "search")["description"], "CUSTOM SEARCH");

    // Keys that were not overridden keep the built-in text.
    let run: &Json = tool(&session, "run");
    assert!(
        run["description"]
            .as_str()
            .unwrap()
            .contains("absolute path")
    );

    let detail: Json = body_of(&session, 3);
    assert_eq!(detail["params"]["prompt"]["required"], true);
}
