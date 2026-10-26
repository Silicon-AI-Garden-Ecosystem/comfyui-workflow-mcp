//! Searching is how a model finds out what exists: first the hosts, then the workflows of
//! one host, then the parameters of one workflow. Each step stays small, and the next step
//! is a hint in the answer rather than a paragraph in the tool description.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Tool};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::config::{self, Descriptions, Param, ParamType, Workflow, Workflows};

use super::{Server, args_schema, fail, text, unknown_host, unknown_workflow};

/// The field help is not written here: `list_tools` overwrites the schema descriptions
/// from `[descriptions]`, so the wording has a single source.
#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SearchArgs {
    #[serde(default)]
    host_name: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
}

#[tool_router(router = search_router, vis = "pub(super)")]
impl Server {
    #[tool]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let loaded = match config::load() {
            Ok(loaded) => loaded,
            Err(err) => return Ok(fail(err)),
        };
        let workflows = &loaded.workflows;
        let descriptions = &loaded.setting.descriptions;

        // A workflow without a host is ambiguous as soon as two hosts share a name, so it
        // is rejected instead of guessing a host.
        let Some(host_name) = args.host_name else {
            if args.workflow_name.is_some() {
                return Ok(fail("workflow_name needs host_name".to_string()));
            }
            return Ok(text(hosts(workflows, descriptions)));
        };

        if workflows.host(&host_name).is_none() {
            return Ok(fail(unknown_host(workflows, &host_name)));
        }

        match args.workflow_name {
            None => Ok(text(host_workflows(workflows, &host_name, descriptions))),
            Some(workflow_name) => match workflows.get(&host_name, &workflow_name) {
                Some(workflow) => Ok(text(detail(workflow))),
                None => Ok(fail(unknown_workflow(
                    workflows,
                    &host_name,
                    &workflow_name,
                ))),
            },
        }
    }
}

pub(super) fn tool(descriptions: &Descriptions) -> Tool {
    Tool::new(
        "search",
        descriptions.search_tool.clone(),
        args_schema::<SearchArgs>(&[
            ("host_name", &descriptions.host_name_arg),
            ("workflow_name", &descriptions.workflow_name_arg),
        ]),
    )
}

/// Only the name and the human description: a host may carry a token or an internal
/// address, and the model has no use for either.
fn hosts(workflows: &Workflows, descriptions: &Descriptions) -> String {
    let items: Vec<Json> = workflows
        .hosts()
        .iter()
        .map(|host| {
            json!({
                "name": host.name,
                "description": host.description,
            })
        })
        .collect();

    json!({
        "hosts": items,
        "hint": descriptions.hint_hosts,
    })
    .to_string()
}

fn host_workflows(workflows: &Workflows, host: &str, descriptions: &Descriptions) -> String {
    let items: Vec<Json> = workflows
        .of_host(host)
        .into_iter()
        .map(|workflow| {
            json!({
                "name": workflow.name,
                "description": workflow.description,
            })
        })
        .collect();

    json!({
        "host": host,
        "workflows": items,
        "hint": descriptions.hint_workflows,
    })
    .to_string()
}

/// The shape is fixed by `sim/trace.md` and stays minimal on purpose: fewer fields mean
/// less for a model to copy wrong, and a drawn value is left out.
fn detail(workflow: &Workflow) -> String {
    let params: Map<String, Json> = workflow
        .manifest
        .params
        .iter()
        .map(|(name, param)| (name.clone(), param_json(param)))
        .collect();

    json!({
        "host": workflow.host,
        "name": workflow.name,
        "description": workflow.description,
        "params": params,
    })
    .to_string()
}

fn param_json(param: &Param) -> Json {
    let mut object = Map::new();
    object.insert("type".to_string(), Json::from(param.ty.as_str()));

    // The wording never mentions random: a model cannot ask for randomness, it can
    // only omit a parameter. The three "what if omitted" variants are mutually exclusive.
    if param.required() {
        object.insert("required".to_string(), Json::Bool(true));
    } else if !param.hooks.is_empty() {
        // The default is the marker that the hook fires on, not a value the model should
        // copy: dropping the input is the template's business, not an argument.
        object.insert("optional".to_string(), Json::Bool(true));
    } else if let Some(default) = &param.default {
        object.insert("default".to_string(), default.clone());
    } else {
        object.insert("optional".to_string(), Json::Bool(true));
    }

    if let Some((min, max)) = param.range {
        // Integer bounds are rendered as integers, float bounds keep their fraction: the
        // numbers the model copies must be the numbers the workflow declared.
        let bounds = match param.ty {
            ParamType::Float => json!([min, max]),
            _ => json!([min as i64, max as i64]),
        };
        object.insert("range".to_string(), bounds);
    }
    if let Some(values) = &param.values {
        object.insert(
            "enum".to_string(),
            Json::Array(values.iter().map(|v| Json::from(v.as_str())).collect()),
        );
    }
    if let Some(description) = &param.description {
        object.insert("description".to_string(), Json::from(description.as_str()));
    }

    Json::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Param;

    fn param(ty: ParamType) -> Param {
        Param {
            target: vec!["1".to_string(), "inputs".to_string(), "x".to_string()],
            ty,
            default: None,
            random: false,
            range: None,
            values: None,
            description: None,
            hooks: Vec::new(),
        }
    }

    #[test]
    fn a_random_parameter_is_optional_and_keeps_its_range() {
        let mut p = param(ParamType::Integer);
        p.random = true;
        p.range = Some((0.0, 9007199254740991.0));
        let json = param_json(&p);
        assert_eq!(json["range"], json!([0i64, 9_007_199_254_740_991i64]));
        assert_eq!(json["optional"], json!(true));
        // The word "random" is not for the model: it cannot ask for randomness, it can
        // only omit a parameter.
        assert!(json.get("random").is_none());
    }

    #[test]
    fn float_range_keeps_fraction() {
        let mut p = param(ParamType::Float);
        p.default = Some(json!(8.0));
        p.range = Some((1.0, 30.0));
        let json = param_json(&p);
        // The text we send must match the TOML, whatever the client renders.
        assert_eq!(json["default"], json!(8.0));
        assert_eq!(json["range"], json!([1.0, 30.0]));
    }

    #[test]
    fn no_default_means_required() {
        let json = param_json(&param(ParamType::String));
        assert_eq!(json["required"], json!(true));
        assert!(json.get("optional").is_none());
        assert!(json.get("default").is_none());
    }
}
