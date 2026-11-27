use super::*;
use crate::config::manifest::{Hook, HookSet, HookWhen, Param};
use serde_json::json;

fn param(target: &[&str], ty: ParamType) -> Param {
    Param {
        target: target.iter().map(|s| s.to_string()).collect(),
        ty,
        default: None,
        random: false,
        range: None,
        values: None,
        description: None,
        hooks: Vec::new(),
    }
}

fn manifest(params: &[(&str, Param)]) -> Manifest {
    Manifest {
        description: "demo".to_string(),
        params: params
            .iter()
            .map(|(name, param)| (name.to_string(), param.clone()))
            .collect(),
    }
}

fn json() -> Json {
    json!({
        "3": {"inputs": {"seed": 1, "steps": 20, "cfg": 8.0}},
        "4": {"inputs": {"model": ["1", 0]}},
        "6": {"inputs": {"text": "old"}},
        "18:7": {"inputs": {"steps": 25, "cfg": 2.5}},
    })
}

fn counter() -> impl FnMut() -> u64 {
    let mut n = 0;
    move || {
        n += 1;
        n
    }
}

#[test]
fn writes_deep_paths_and_subgraph_ids() {
    let m = manifest(&[
        ("prompt", param(&["6", "inputs", "text"], ParamType::String)),
        (
            "steps",
            param(&["18:7", "inputs", "steps"], ParamType::Integer),
        ),
    ]);
    let params = json!({"prompt": "a cat", "steps": 30});
    let mut rng = counter();
    let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
    assert_eq!(out["6"]["inputs"]["text"], json!("a cat"));
    assert_eq!(out["18:7"]["inputs"]["steps"], json!(30));
}

#[test]
fn unknown_param_lists_all_allowed_names_sorted() {
    let m = manifest(&[
        ("beta", param(&["6", "inputs", "text"], ParamType::String)),
        (
            "alpha",
            param(&["3", "inputs", "steps"], ParamType::Integer),
        ),
    ]);
    let params = json!({"zeta": 1, "gamma": 2});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("an unknown parameter must be rejected");
    assert_eq!(
        err,
        ParamError::Unknown {
            unknown: vec!["gamma".to_string(), "zeta".to_string()],
            allowed: vec!["alpha".to_string(), "beta".to_string()],
        }
    );
}

#[test]
fn missing_required_lists_every_item_with_description() {
    let mut a = param(&["6", "inputs", "text"], ParamType::String);
    a.description = Some("positive prompt".to_string());
    let b = param(&["3", "inputs", "steps"], ParamType::Integer);
    let m = manifest(&[("prompt", a), ("steps", b)]);
    let mut rng = counter();
    let err = resolve(&m, &json(), None, &mut rng)
        .expect_err("a missing required parameter must be rejected");
    assert_eq!(
        err,
        ParamError::Missing {
            items: vec![
                ("prompt".to_string(), Some("positive prompt".to_string())),
                ("steps".to_string(), None),
            ],
        }
    );
}

#[test]
fn default_is_written_when_omitted() {
    let mut p = param(&["3", "inputs", "steps"], ParamType::Integer);
    p.default = Some(json!(28));
    let m = manifest(&[("steps", p)]);
    let template = json();
    let mut rng = counter();
    let out = resolve(&m, &template, None, &mut rng).unwrap();
    assert_eq!(out["3"]["inputs"]["steps"], json!(28));
    assert_eq!(
        template["3"]["inputs"]["steps"],
        json!(20),
        "the workflow JSON must not be mutated"
    );
}

#[test]
fn float_accepts_integer_and_writes_float() {
    let m = manifest(&[("cfg", param(&["3", "inputs", "cfg"], ParamType::Float))]);
    let params = json!({"cfg": 8});
    let mut rng = counter();
    let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
    assert_eq!(out["3"]["inputs"]["cfg"], json!(8.0));
    assert!(out["3"]["inputs"]["cfg"].is_f64());
}

#[test]
fn integer_rejects_float_and_string() {
    let m = manifest(&[(
        "steps",
        param(&["3", "inputs", "steps"], ParamType::Integer),
    )]);

    let params = json!({"steps": 8.0});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("integer must not accept 8.0");
    assert_eq!(
        err,
        ParamError::TypeMismatch {
            param: "steps".to_string(),
            expected: "integer",
            got: "float",
        }
    );

    let params = json!({"steps": "8"});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("integer must not accept a string");
    assert_eq!(
        err,
        ParamError::TypeMismatch {
            param: "steps".to_string(),
            expected: "integer",
            got: "string",
        }
    );
}

#[test]
fn explicit_zero_beats_random() {
    let mut p = param(&["3", "inputs", "seed"], ParamType::Integer);
    p.random = true;
    p.range = Some((0.0, 100.0));
    let m = manifest(&[("seed", p)]);
    let params = json!({"seed": 0});
    let mut rng = counter();
    let out = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng).unwrap();
    assert_eq!(out["3"]["inputs"]["seed"], json!(0));
}

#[test]
fn random_integer_stays_in_range_and_differs_across_calls() {
    let mut p = param(&["3", "inputs", "seed"], ParamType::Integer);
    p.random = true;
    p.range = Some((10.0, 20.0));
    let m = manifest(&[("seed", p)]);
    let mut rng = counter();

    let first = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["seed"]
        .as_i64()
        .expect("integer");
    let second = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["seed"]
        .as_i64()
        .expect("integer");
    assert!((10..=20).contains(&first), "{first}");
    assert!((10..=20).contains(&second), "{second}");
    assert_ne!(first, second, "two draws must differ");
}

#[test]
fn random_float_uses_range() {
    let mut p = param(&["3", "inputs", "cfg"], ParamType::Float);
    p.random = true;
    p.range = Some((1.0, 30.0));
    let m = manifest(&[("cfg", p)]);
    let mut rng = counter();
    for _ in 0..5 {
        let value = resolve(&m, &json(), None, &mut rng).unwrap()["3"]["inputs"]["cfg"]
            .as_f64()
            .expect("float");
        assert!((1.0..=30.0).contains(&value), "{value}");
    }
}

#[test]
fn explicit_value_outside_range_or_enum_is_rejected() {
    let mut p = param(&["3", "inputs", "steps"], ParamType::Integer);
    p.range = Some((1.0, 100.0));
    let m = manifest(&[("steps", p)]);
    let params = json!({"steps": 200});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("out-of-range must be rejected");
    assert_eq!(
        err,
        ParamError::OutOfRange {
            param: "steps".to_string(),
            min: 1.0,
            max: 100.0,
            got: 200.0,
        }
    );

    let mut p = param(&["6", "inputs", "text"], ParamType::String);
    p.values = Some(vec!["a".to_string(), "b".to_string()]);
    let m = manifest(&[("prompt", p)]);
    let params = json!({"prompt": "c"});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("a value outside enum must be rejected");
    assert_eq!(
        err,
        ParamError::NotInEnum {
            param: "prompt".to_string(),
            allowed: vec!["a".to_string(), "b".to_string()],
            got: "c".to_string(),
        }
    );
}

#[test]
fn writing_to_connection_or_missing_path_is_bad_target() {
    let m = manifest(&[("model", param(&["4", "inputs", "model"], ParamType::String))]);
    let params = json!({"model": "x"});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("a link is not a parameter");
    assert_eq!(
        err,
        ParamError::BadTarget {
            param: "model".to_string(),
            pointer: "/4/inputs/model".to_string(),
        }
    );

    let m = manifest(&[("x", param(&["9", "inputs", "x"], ParamType::Integer))]);
    let params = json!({"x": 1});
    let mut rng = counter();
    let err = resolve(&m, &json(), Some(params.as_object().unwrap()), &mut rng)
        .expect_err("the path does not exist");
    assert_eq!(
        err,
        ParamError::BadTarget {
            param: "x".to_string(),
            pointer: "/9/inputs/x".to_string(),
        }
    );
}

#[test]
fn pointer_segments_are_escaped() {
    let p = param(&["a/b", "c~d"], ParamType::String);
    assert_eq!(p.pointer(), "/a~1b/c~0d");
}

#[test]
fn a_file_parameter_is_left_for_the_upload_path() {
    let m = manifest(&[("person", param(&["6", "inputs", "image"], ParamType::File))]);
    let mut with_file = json();
    with_file["6"]["inputs"]["image"] = json!("old.png");
    let params = json!({"person": "~/pics/me.png"});
    let mut rng = counter();
    let resolved = resolve(&m, &with_file, Some(params.as_object().unwrap()), &mut rng).unwrap();

    // The JSON keeps the value it had: the name only exists once the file is hashed.
    assert_eq!(resolved.json["6"]["inputs"]["image"], json!("old.png"));
    assert_eq!(
        resolved.files,
        vec![FileSlot {
            pointer: "/6/inputs/image".to_string(),
            path: "~/pics/me.png".to_string(),
        }]
    );

    // A default is a path like any other, so it follows the same route.
    let mut file = param(&["6", "inputs", "image"], ParamType::File);
    file.default = Some(json!("~/pics/default.png"));
    let m = manifest(&[("person", file)]);
    let resolved = resolve(&m, &json(), None, &mut counter()).unwrap();
    assert_eq!(resolved.files.len(), 1);
    assert_eq!(resolved.files[0].path, "~/pics/default.png");
}

#[test]
fn apply_files_writes_the_names_the_server_returned() {
    let mut workflow = json();
    workflow["6"]["inputs"]["image"] = json!("old-a.png");
    workflow["9"] = json!({"inputs": {"image": "old-b.png"}});
    let named = vec![
        ("/6/inputs/image".to_string(), "abc.png".to_string()),
        ("/9/inputs/image".to_string(), "def.png".to_string()),
    ];
    apply_files(&mut workflow, &named).unwrap();
    assert_eq!(workflow["6"]["inputs"]["image"], json!("abc.png"));
    assert_eq!(workflow["9"]["inputs"]["image"], json!("def.png"));
}

#[test]
fn apply_files_rejects_a_pointer_that_no_longer_exists() {
    let mut workflow = json();
    let named = vec![("/6/inputs/other".to_string(), "abc.png".to_string())];
    assert_eq!(
        apply_files(&mut workflow, &named),
        Err("/6/inputs/other".into())
    );
}

fn hooked_param() -> Param {
    let mut param = param(&["a", "inputs", "x"], ParamType::String);
    // A hook only runs on the default branch, so it needs a default to fire on.
    param.default = Some(json!(""));
    param.hooks = vec![Hook {
        when: HookWhen::Default,
        remove: vec![vec![
            "a".to_string(),
            "inputs".to_string(),
            "link".to_string(),
        ]],
        set: vec![HookSet {
            target: vec!["a".to_string(), "inputs".to_string(), "flag".to_string()],
            value: json!(false),
        }],
    }];
    param
}

#[test]
fn omitting_a_hooked_parameter_removes_the_link_and_sets_constants() {
    let manifest = manifest(&[("p", hooked_param())]);
    let json = json!({"a": {"inputs": {"x": "old", "link": ["9", 0], "flag": true}}});
    let out = resolve(&manifest, &json, None, &mut || 0).unwrap();

    assert!(out.json["a"]["inputs"].get("link").is_none());
    assert_eq!(out.json["a"]["inputs"]["flag"], json!(false));
    assert_eq!(
        out.json["a"]["inputs"]["x"],
        json!(""),
        "the default is written first; the hook decides what survives"
    );
}

#[test]
fn a_hook_can_overwrite_the_default_written_before_it() {
    let mut param = param(&["a", "inputs", "x"], ParamType::Integer);
    param.default = Some(json!(1));
    param.hooks = vec![Hook {
        when: HookWhen::Default,
        remove: Vec::new(),
        set: vec![HookSet {
            target: vec!["a".to_string(), "inputs".to_string(), "x".to_string()],
            value: json!(5),
        }],
    }];
    let manifest = manifest(&[("p", param)]);

    let out = resolve(
        &manifest,
        &json!({"a": {"inputs": {"x": 0}}}),
        None,
        &mut || 0,
    )
    .unwrap();
    assert_eq!(out.json["a"]["inputs"]["x"], json!(5));
}

#[test]
fn an_empty_file_default_creates_no_upload() {
    let mut file = param(&["6", "inputs", "image"], ParamType::File);
    file.default = Some(json!(""));
    let manifest = manifest(&[("person", file)]);

    let resolved = resolve(&manifest, &json(), None, &mut || 0).unwrap();
    assert!(resolved.files.is_empty(), "{:?}", resolved.files);
}

#[test]
fn a_provided_parameter_skips_its_hook() {
    let manifest = manifest(&[("p", hooked_param())]);
    let json = json!({"a": {"inputs": {"x": "old", "link": ["9", 0], "flag": true}}});
    let params = json!({"p": "new"});
    let out = resolve(&manifest, &json, params.as_object(), &mut || 0).unwrap();

    assert_eq!(out.json["a"]["inputs"]["link"], json!(["9", 0]));
    assert_eq!(out.json["a"]["inputs"]["flag"], json!(true));
    assert_eq!(out.json["a"]["inputs"]["x"], json!("new"));
}
