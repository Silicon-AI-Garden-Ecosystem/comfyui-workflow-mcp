//! Every word the model reads lives in `[messages]`, next to the descriptions: the same
//! configuration that names the tools also owns their failures. Nothing here is a default —
//! `builtin()` reads the compiled-in `sim/config.toml`, so the file stays the only source.

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    pub list_separator: String,
    pub params: ParamMessages,
    pub files: FileMessages,
    pub comfy: ComfyMessages,
    pub run: RunMessages,
    pub tools: ToolMessages,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamMessages {
    pub missing: String,
    pub missing_with_description: String,
    pub missing_bare: String,
    pub unknown: String,
    pub quoted: String,
    pub type_mismatch: String,
    pub out_of_range: String,
    pub not_in_enum: String,
    pub bad_target: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMessages {
    pub not_absolute: String,
    pub no_extension: String,
    pub not_found: String,
    pub not_a_file: String,
    pub read: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComfyMessages {
    pub transport: String,
    pub timeout: String,
    pub raw: String,
    pub http_status: String,
    pub missing_file: String,
    pub stage_submit: String,
    pub stage_execution: String,
    pub stage_download: String,
    pub stage_upload: String,
    pub stage_check: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMessages {
    pub timeout: String,
    pub stage_upload: String,
    pub stage_submit: String,
    pub stage_download: String,
    pub missing_field: String,
    pub io: String,
    pub missing_upload_name: String,
    pub multiple_separator: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMessages {
    pub unknown_host: String,
    pub unknown_host_without_hosts: String,
    pub unknown_workflow: String,
    pub unknown_workflow_without_workflows: String,
    pub workflow_name_needs_host: String,
    pub output_path_empty: String,
    pub output_path_not_absolute: String,
}

/// Replaces `{name}` with the matching argument and leaves unknown placeholders untouched:
/// a typo must be visible in the answer, not silently deleted.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        match args.iter().find(|(name, _)| *name == key) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn builtin() -> &'static Messages {
    static BUILTIN: OnceLock<Messages> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        #[derive(Deserialize)]
        struct Builtin {
            messages: Messages,
        }
        toml::from_str::<Builtin>(include_str!("../../sim/config.toml"))
            .expect("sim/config.toml must parse")
            .messages
    })
}

#[cfg(test)]
mod tests {
    use super::{builtin, fill};

    #[test]
    fn every_placeholder_is_replaced() {
        assert_eq!(
            fill("a {one} b {two} c", &[("one", "1"), ("two", "2")]),
            "a 1 b 2 c"
        );
    }

    #[test]
    fn an_unknown_placeholder_stays_visible() {
        assert_eq!(fill("x {missing} y", &[]), "x {missing} y");
    }

    #[test]
    fn the_builtin_table_parses() {
        assert!(
            builtin()
                .tools
                .workflow_name_needs_host
                .contains("host_name")
        );
    }
}
