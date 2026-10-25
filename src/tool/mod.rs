//! The tool surface is two tools and three signatures: fewer fields for the model to copy
//! means fewer ways to copy wrong. The server holds no state either, so every call
//! re-reads the configuration and rescans the workflows directory.

mod run;
mod search;

use std::process::ExitCode;

use rmcp::handler::server::common::schema_for_input;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams};
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool_handler, transport::stdio};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::config::{self, Descriptions, Workflows};

pub async fn serve() -> ExitCode {
    let server = Server;

    // `stdio()` and not `stdio_pipes()`: it works on Windows too and needs no pipe of our
    // own. stdout carries protocol only, so every log line goes to stderr.
    match server.serve(stdio()).await {
        Ok(service) => match service.waiting().await {
            Ok(_) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("MCP session error: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("failed to start the MCP server: {err}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) struct Server;

/// Each tool file brings its own router; the server hands out the merged one.
fn router() -> ToolRouter<Server> {
    let mut router = Server::search_router();
    router.merge(Server::run_router());
    router
}

#[tool_handler(name = "comfyui-workflow-mcp", router = router())]
impl ServerHandler for Server {
    /// Hand-written because the wording is configuration: the tool descriptions and the
    /// parameter help are injected from `[descriptions]`, so they can be tuned without
    /// recompiling. A broken configuration falls back to the built-in text, read from the
    /// same `sim/config.toml`.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let descriptions = match config::load() {
            Ok(config) => config.setting.descriptions,
            Err(_) => builtin_descriptions(),
        };

        // Sorted by name, like the macro-generated list: repeated calls must hand the
        // client the same order.
        let mut tools = vec![search::tool(&descriptions), run::tool(&descriptions)];
        tools.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }
}

#[derive(Deserialize)]
struct BuiltinConfig {
    descriptions: Descriptions,
}

fn builtin_descriptions() -> Descriptions {
    toml::from_str::<BuiltinConfig>(include_str!("../../sim/config.toml"))
        .expect("sim/config.toml must parse")
        .descriptions
}

fn args_schema<T: JsonSchema + std::any::Any>(
    properties: &[(&str, &str)],
) -> serde_json::Map<String, Json> {
    let Ok(schema) = schema_for_input::<Parameters<T>>() else {
        return serde_json::Map::new();
    };
    let mut schema = (*schema).clone();
    for (name, description) in properties {
        if let Some(field) = schema
            .get_mut("properties")
            .and_then(Json::as_object_mut)
            .and_then(|properties| properties.get_mut(*name))
            .and_then(Json::as_object_mut)
        {
            field.insert("description".to_string(), Json::from(*description));
        }
    }
    schema
}

/// An unknown name lists every available name: the number of workflows is single digit,
/// so listing them beats fuzzy matching, which would quietly run a different workflow.
fn unknown(workflows: &Workflows, name: &str) -> String {
    let names = workflows.names();
    if names.is_empty() {
        return format!("unknown workflow \"{name}\": no workflows are available");
    }
    format!(
        "unknown workflow \"{name}\". Available: {}",
        names.join(", ")
    )
}

fn text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(body.into())])
}

fn fail(body: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(body.into())])
}
