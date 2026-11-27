use super::*;
use crate::config::manifest::test_support::*;
use serde_json::json;

#[test]
fn rejects_unknown_keys_with_position() {
    let err = parse_err("description = \"d\"\nextra = 1\n");
    assert!(err.message.contains("extra"), "{}", err.message);
    assert_eq!(err.at, "workflow.toml:2");

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nextra = true\n",
    ));
    assert!(err.message.contains("extra"), "{}", err.message);
    assert!(err.at.starts_with("workflow.toml:"), "{}", err.at);
}

#[test]
fn description_must_be_present_and_non_empty() {
    let err = parse_err("params = {}\n");
    assert!(err.message.contains("description"), "{}", err.message);

    let err = parse_err("description = \"  \"\n");
    assert!(
        err.message.contains("description must not be empty"),
        "{}",
        err.message
    );
}

#[test]
fn target_must_be_non_empty_segments_of_strings() {
    let err = parse_err(&param_toml("target = []\ntype = \"integer\"\n"));
    assert!(
        err.message.contains("target must not be empty"),
        "{}",
        err.message
    );
    assert_eq!(err.at, "workflow.toml:4");

    let err = parse_err(&param_toml("target = [\"3\", \"\"]\ntype = \"integer\"\n"));
    assert!(
        err.message.contains("empty path segment"),
        "{}",
        err.message
    );
}

#[test]
fn type_must_be_one_of_four() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"int\"\n",
    ));
    assert!(err.message.contains("is invalid"), "{}", err.message);
    assert!(err.message.contains("string / integer / float / boolean"));
    assert_eq!(err.at, "workflow.toml:5");
}

#[test]
fn required_key_is_no_longer_accepted() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrequired = true\n",
    ));
    assert!(err.message.contains("unknown field"), "{}", err.message);
    assert_eq!(err.at, "workflow.toml:6");
}

#[test]
fn integer_range_must_fit_safe_integers() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\nrange = [0, 9223372036854775807]\n",
    ));
    assert!(
        err.message.contains("safe-integer limit"),
        "{}",
        err.message
    );

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\nrange = [0.5, 10.5]\n",
    ));
    assert!(
        err.message.contains("needs integer bounds"),
        "{}",
        err.message
    );
}

#[test]
fn range_must_be_two_numbers_on_numeric_params() {
    let err = parse_err(&param_toml(
        "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nrange = [1, 2]\n",
    ));
    assert!(
        err.message.contains("range is only valid for numeric"),
        "{}",
        err.message
    );

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [1]\n",
    ));
    assert!(
        err.message.contains("must be [min, max]"),
        "{}",
        err.message
    );

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [5, 1]\n",
    ));
    assert!(err.message.contains("greater than"), "{}", err.message);

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nrange = [\"a\", 1]\n",
    ));
    assert!(err.message.contains("is not a number"), "{}", err.message);
}

#[test]
fn enum_only_on_strings_with_string_values() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\nenum = [\"a\"]\n",
    ));
    assert!(
        err.message.contains("enum is only valid for"),
        "{}",
        err.message
    );

    let err = parse_err(&param_toml(
        "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nenum = [\"a\", 2]\n",
    ));
    assert!(err.message.contains("must be strings"), "{}", err.message);

    let err = parse_err(&param_toml(
        "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\nrange = [1, 2]\nenum = [\"a\"]\n",
    ));
    assert!(err.message.contains("range and enum"), "{}", err.message);
}

#[test]
fn default_must_match_type() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = \"28\"\n",
    ));
    assert!(err.message.contains("does not match"), "{}", err.message);
    assert!(err.message.contains("string"), "{}", err.message);

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = 1.5\n",
    ));
    assert!(err.message.contains("does not match"), "{}", err.message);

    let err = parse_err(&param_toml(
        "target = [\"6\", \"inputs\", \"text\"]\ntype = \"string\"\ndefault = 1\n",
    ));
    assert!(err.message.contains("does not match"), "{}", err.message);
}

#[test]
fn float_default_accepts_integer_and_normalizes() {
    let m = parse(&param_toml(
        "target = [\"3\", \"inputs\", \"cfg\"]\ntype = \"float\"\ndefault = 8\n",
    ))
    .expect("a float accepts an integer");
    let default = m.params["p"].default.clone().expect("has a default");
    assert_eq!(default, json!(8.0));
    assert!(default.is_f64(), "must normalize to 8.0: {default}");
}

#[test]
fn default_must_be_inside_range_and_enum() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"steps\"]\ntype = \"integer\"\ndefault = 200\nrange = [1, 100]\n",
    ));
    assert!(err.message.contains("outside range"), "{}", err.message);

    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"sampler_name\"]\ntype = \"string\"\ndefault = \"x\"\nenum = [\"a\", \"b\"]\n",
    ));
    assert!(err.message.contains("is not in enum"), "{}", err.message);
}

#[test]
fn a_hook_is_parsed_from_the_sample() {
    let m = parse(include_str!(
        "../../../../sim/workflows/local/image_qwen_image_2_1_image_edit.toml"
    ))
    .unwrap();
    let image2 = &m.params["image_2"];
    assert_eq!(image2.hooks.len(), 1);
    assert_eq!(image2.hooks[0].when, HookWhen::Default);
    assert_eq!(
        image2.hooks[0].remove,
        vec![vec![
            "459:474".to_string(),
            "inputs".to_string(),
            "images.image_2".to_string()
        ]]
    );
    assert_eq!(image2.hooks[0].set.len(), 1);
    assert_eq!(image2.hooks[0].set[0].value, json!(false));
    // The default is the marker the hook fires on, it is never written to the JSON.
    assert!(!image2.required());
    assert_eq!(image2.default, Some(json!("")));
}

#[test]
fn hook_contract_is_enforced() {
    let when = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"always\"\nremove = [[\"3\", \"inputs\", \"steps\"]]\n",
    ));
    assert!(
        when.message.contains("is not supported"),
        "{}",
        when.message
    );

    let empty = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\n",
    ));
    assert!(
        empty.message.contains("needs remove or set"),
        "{}",
        empty.message
    );

    let list = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\nset = [{ target = [\"3\", \"inputs\", \"steps\"], value = [1, 2] }]\n",
    ));
    assert!(
        list.message.contains("must be a scalar"),
        "{}",
        list.message
    );
}

#[test]
fn a_hook_requires_a_default() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\", \"steps\"]]\n",
    ));
    assert!(
        err.message.contains("hook needs default"),
        "{}",
        err.message
    );
}

#[test]
fn a_hook_may_not_touch_a_parameter_target() {
    let err = parse_err(
        "description = \"d\"\n\n[params.p]\ntarget = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\"]]\n",
    );
    assert!(
        err.message.contains("contains params.p's target"),
        "{}",
        err.message
    );
}

#[test]
fn a_hook_set_may_not_live_inside_its_remove() {
    let err = parse_err(&param_toml(
        "target = [\"3\", \"inputs\", \"seed\"]\ntype = \"integer\"\ndefault = 1\n[[params.p.hook]]\nwhen = \"default\"\nremove = [[\"3\", \"inputs\", \"cfg\"]]\nset = [{ target = [\"3\", \"inputs\", \"cfg\", \"x\"], value = 1 }]\n",
    ));
    assert!(
        err.message.contains("inside the removed"),
        "{}",
        err.message
    );
}
