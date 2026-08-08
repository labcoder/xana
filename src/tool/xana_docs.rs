use super::{EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition};
use crate::{
    permission::PermissionScope,
    self_docs::{DocRange, default_catalog},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) struct XanaDocs;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "op", rename_all = "snake_case")]
enum Args {
    List {
        topic: Option<String>,
    },
    Read {
        id: String,
        start: Option<usize>,
        max_bytes: Option<usize>,
    },
}

impl Tool for XanaDocs {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "xana_docs",
            contract_version: 1,
            description: "List or read bounded, version-matched documentation about Xana itself",
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["op"],
                "properties":{
                    "op":{"type":"string","enum":["list","read"]},
                    "topic":{"type":"string"},
                    "id":{"type":"string"},
                    "start":{"type":"integer","minimum":0},
                    "max_bytes":{"type":"integer","minimum":1,"maximum":32768}
                }
            }),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: Args = serde_json::from_value(arguments.clone())
            .map_err(|_| "xana_docs arguments are invalid".to_owned())?;
        let final_arguments = serde_json::to_value(&args).map_err(|error| error.to_string())?;
        Ok(PlannedToolInvocation::new(
            final_arguments,
            PermissionScope::Unscoped,
            args,
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<Args>("xana_docs")?;
            let catalog = default_catalog();
            match args {
                Args::List { topic } => serde_json::to_string(&catalog.list(topic.as_deref()))
                    .map_err(|error| error.to_string()),
                Args::Read {
                    id,
                    start,
                    max_bytes,
                } => {
                    let range = start.or(*max_bytes).map(|_| DocRange {
                        start: start.unwrap_or(0),
                        max_bytes: max_bytes.unwrap_or(32 * 1024),
                    });
                    let document = catalog.read(id, range).map_err(|error| error.to_string())?;
                    serde_json::to_string(&json!({
                        "id":document.id,
                        "text":document.text,
                        "start":document.range.start,
                        "end":document.range.end,
                        "truncated":document.truncated
                    }))
                    .map_err(|error| error.to_string())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_is_bounded_and_product_specific() {
        let definition = XanaDocs.definition();
        assert_eq!(definition.name, "xana_docs");
        assert_eq!(
            definition.parameters["properties"]["max_bytes"]["maximum"],
            32768
        );
    }
}
