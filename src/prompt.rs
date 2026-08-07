//! Versioned, provider-neutral system-prompt assembly.
//!
//! The application edge supplies every dynamic input. Assembly freezes one
//! byte-stable system message for an agent; providers only serialize it.

mod render;

use crate::{
    context::{
        ContextBudget, ContextError, ContextPlan, ContextPlanner, ContextSource, SourceOrigin,
        SourceProvenance, TrustClass, estimate_tokens,
    },
    message::{Message, Role},
    tool::ToolDefinition,
};

use render::{
    estimate_message_tokens, estimate_tool_schema_tokens, refresh_layer_costs, render_layers,
    trim_outer_blank_lines,
};
use std::{collections::HashSet, error::Error, fmt, path::PathBuf};

pub(crate) const PROMPT_ASSEMBLY_VERSION: &str = "xana-prompt-v1";

const IDENTITY: &str = include_str!("prompt/identity.md");
const GUIDELINES: &str = include_str!("prompt/guidelines.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptLayerKind {
    Identity,
    Guidelines,
    ProductDocumentation,
    ToolCatalog,
    Environment,
    Surface,
    ProjectInstructions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptSurface {
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptEnvironment {
    pub(crate) operating_system: String,
    pub(crate) working_directory: PathBuf,
    pub(crate) configured_shell: String,
    pub(crate) surface: PromptSurface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductDocumentationHint {
    pub(crate) capability: String,
    pub(crate) references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptLayer {
    pub(crate) kind: PromptLayerKind,
    pub(crate) source_id: String,
    pub(crate) provenance: SourceProvenance,
    pub(crate) trust: TrustClass,
    pub(crate) text: String,
    pub(crate) estimated_tokens: usize,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PromptSnapshot {
    pub(crate) version: &'static str,
    pub(crate) system_message: Message,
    pub(crate) layers: Vec<PromptLayer>,
    pub(crate) system_tokens: usize,
    pub(crate) tool_schema_tokens: usize,
    pub(crate) budget: ContextBudget,
    pub(crate) context_plan: ContextPlan,
}

impl PromptSnapshot {
    pub(crate) fn messages_for_request(
        &self,
        history: &[Message],
    ) -> Result<Vec<Message>, PromptError> {
        if history.iter().any(|message| message.role == Role::System) {
            return Err(PromptError::SystemRoleInHistory);
        }

        let history_tokens = history.iter().map(estimate_message_tokens).sum::<usize>();
        let used = self
            .system_tokens
            .saturating_add(self.tool_schema_tokens)
            .saturating_add(history_tokens);
        if used > self.budget.total_tokens {
            return Err(PromptError::HistoryExceedsBudget {
                system_tokens: self.system_tokens,
                tool_schema_tokens: self.tool_schema_tokens,
                history_tokens,
                total_tokens: self.budget.total_tokens,
            });
        }

        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(self.system_message.clone());
        messages.extend_from_slice(history);
        Ok(messages)
    }
}

pub(crate) struct PromptInputs<'a> {
    pub(crate) tool_definitions: &'a [&'a ToolDefinition],
    pub(crate) environment: &'a PromptEnvironment,
    pub(crate) product_documentation: Option<&'a ProductDocumentationHint>,
    pub(crate) project_sources: &'a [ContextSource],
    pub(crate) budget: ContextBudget,
}

#[derive(Debug)]
pub(crate) enum PromptError {
    Context(ContextError),
    InvalidEnvironment {
        field: &'static str,
    },
    InvalidProductDocumentation {
        reason: &'static str,
    },
    DuplicateSourceId {
        id: String,
    },
    RequiredLayersExceedBudget {
        required_tokens: usize,
        total_tokens: usize,
    },
    HistoryExceedsBudget {
        system_tokens: usize,
        tool_schema_tokens: usize,
        history_tokens: usize,
        total_tokens: usize,
    },
    SystemRoleInHistory,
}

pub(crate) fn assemble_snapshot(inputs: PromptInputs<'_>) -> Result<PromptSnapshot, PromptError> {
    validate_environment(inputs.environment)?;

    let mut layers = vec![
        layer(
            PromptLayerKind::Identity,
            "builtin:identity",
            SourceProvenance {
                display_name: "Xana identity v1".to_owned(),
                path: None,
                origin: SourceOrigin::BuiltIn,
            },
            TrustClass::Xana,
            IDENTITY,
            false,
        ),
        layer(
            PromptLayerKind::Guidelines,
            "builtin:guidelines",
            SourceProvenance {
                display_name: "Xana operating guidelines v1".to_owned(),
                path: None,
                origin: SourceOrigin::BuiltIn,
            },
            TrustClass::Xana,
            GUIDELINES,
            false,
        ),
    ];

    if let Some(documentation) = inputs.product_documentation {
        if documentation.capability.trim().is_empty() {
            return Err(PromptError::InvalidProductDocumentation {
                reason: "capability must not be blank",
            });
        }
        if documentation
            .references
            .iter()
            .any(|reference| reference.trim().is_empty())
        {
            return Err(PromptError::InvalidProductDocumentation {
                reason: "references must not be blank",
            });
        }

        let references = if documentation.references.is_empty() {
            "No logical document references were supplied.".to_owned()
        } else {
            documentation
                .references
                .iter()
                .map(|reference| format!("- {reference}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        layers.push(layer(
            PromptLayerKind::ProductDocumentation,
            "product:xana-documentation",
            SourceProvenance {
                display_name: "available Xana documentation".to_owned(),
                path: None,
                origin: SourceOrigin::ProductDocumentation,
            },
            TrustClass::Xana,
            &format!(
                "Xana documentation is available through capability {}. Logical references:\n{}",
                documentation.capability, references
            ),
            false,
        ));
    }

    layers.push(layer(
        PromptLayerKind::ToolCatalog,
        "tools:catalog",
        SourceProvenance {
            display_name: "immutable tool registry snapshot".to_owned(),
            path: None,
            origin: SourceOrigin::ToolRegistry,
        },
        TrustClass::Runtime,
        &tool_catalog(inputs.tool_definitions),
        false,
    ));
    layers.push(layer(
        PromptLayerKind::Environment,
        "runtime:environment",
        SourceProvenance {
            display_name: "owned runtime environment".to_owned(),
            path: None,
            origin: SourceOrigin::RuntimeEnvironment,
        },
        TrustClass::Runtime,
        &format!(
            "Operating system: {}\nWorking directory: {}\nConfigured shell: {}",
            inputs.environment.operating_system,
            inputs.environment.working_directory.display(),
            inputs.environment.configured_shell
        ),
        false,
    ));
    layers.push(layer(
        PromptLayerKind::Surface,
        "runtime:surface",
        SourceProvenance {
            display_name: "active Xana surface".to_owned(),
            path: None,
            origin: SourceOrigin::RuntimeEnvironment,
        },
        TrustClass::Runtime,
        match inputs.environment.surface {
            PromptSurface::Cli => {
                "Surface: Xana CLI. Communicate in terminal-friendly text and do not claim graphical or computer-control capabilities."
            }
        },
        false,
    ));

    validate_unique_layer_ids(&layers)?;
    refresh_layer_costs(&mut layers);
    let tool_schema_tokens = estimate_tool_schema_tokens(inputs.tool_definitions);
    let fixed_text = render_layers(&layers);
    let fixed_system_tokens = estimate_tokens(&fixed_text);
    let required_tokens = fixed_system_tokens
        .saturating_add(tool_schema_tokens)
        .saturating_add(inputs.budget.conversation_reserve_tokens);
    if required_tokens > inputs.budget.total_tokens {
        return Err(PromptError::RequiredLayersExceedBudget {
            required_tokens,
            total_tokens: inputs.budget.total_tokens,
        });
    }

    let planner = ContextPlanner::new(inputs.project_sources.to_vec(), inputs.budget)
        .map_err(PromptError::Context)?;
    let planned = planner
        .plan(fixed_system_tokens + tool_schema_tokens)
        .map_err(PromptError::Context)?;
    let mut context_plan = ContextPlan {
        selected: Vec::new(),
        used_tokens: 0,
        omitted_sources: planned.omitted_sources,
    };

    for preview in planned.selected {
        let project_layer = layer(
            PromptLayerKind::ProjectInstructions,
            preview.source_id.as_str(),
            preview.provenance.clone(),
            preview.trust,
            &preview.text,
            preview.truncated,
        );
        layers.push(project_layer);
        refresh_layer_costs(&mut layers);
        let candidate_text = render_layers(&layers);
        let candidate_tokens = estimate_tokens(&candidate_text);
        let candidate_total = candidate_tokens
            .saturating_add(tool_schema_tokens)
            .saturating_add(inputs.budget.conversation_reserve_tokens);

        if candidate_total <= inputs.budget.total_tokens {
            context_plan.used_tokens += preview.estimated_tokens;
            context_plan.selected.push(preview);
        } else {
            layers.pop();
            context_plan.omitted_sources.push(preview.source_id);
        }
    }

    validate_unique_layer_ids(&layers)?;
    refresh_layer_costs(&mut layers);
    let rendered = render_layers(&layers);
    let system_tokens = estimate_tokens(&rendered);

    Ok(PromptSnapshot {
        version: PROMPT_ASSEMBLY_VERSION,
        system_message: Message::text(Role::System, rendered),
        layers,
        system_tokens,
        tool_schema_tokens,
        budget: inputs.budget,
        context_plan,
    })
}

fn layer(
    kind: PromptLayerKind,
    source_id: impl Into<String>,
    provenance: SourceProvenance,
    trust: TrustClass,
    text: &str,
    truncated: bool,
) -> PromptLayer {
    PromptLayer {
        kind,
        source_id: source_id.into(),
        provenance,
        trust,
        text: trim_outer_blank_lines(text),
        estimated_tokens: 0,
        truncated,
    }
}

fn validate_environment(environment: &PromptEnvironment) -> Result<(), PromptError> {
    for (field, value) in [
        ("operating_system", environment.operating_system.as_str()),
        ("configured_shell", environment.configured_shell.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(PromptError::InvalidEnvironment { field });
        }
    }
    if environment.working_directory.as_os_str().is_empty() {
        return Err(PromptError::InvalidEnvironment {
            field: "working_directory",
        });
    }
    Ok(())
}

fn validate_unique_layer_ids(layers: &[PromptLayer]) -> Result<(), PromptError> {
    let mut ids = HashSet::new();
    for layer in layers {
        if !ids.insert(layer.source_id.as_str()) {
            return Err(PromptError::DuplicateSourceId {
                id: layer.source_id.clone(),
            });
        }
    }
    Ok(())
}

fn tool_catalog(definitions: &[&ToolDefinition]) -> String {
    let mut lines = vec![
        "Available tools for this agent session (exact schemas are supplied separately):"
            .to_owned(),
    ];
    lines.extend(
        definitions
            .iter()
            .map(|definition| format!("- {}: {}", definition.name, definition.description)),
    );
    if definitions.is_empty() {
        lines.push("- none".to_owned());
    }
    lines.join("\n")
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(source) => write!(f, "could not plan prompt context: {source}"),
            Self::InvalidEnvironment { field } => {
                write!(f, "prompt environment field {field} must not be blank")
            }
            Self::InvalidProductDocumentation { reason } => {
                write!(f, "invalid product documentation hint: {reason}")
            }
            Self::DuplicateSourceId { id } => write!(f, "prompt source id {id:?} is duplicated"),
            Self::RequiredLayersExceedBudget {
                required_tokens,
                total_tokens,
            } => write!(
                f,
                "required prompt layers need {required_tokens} estimated tokens but the total budget is {total_tokens}"
            ),
            Self::HistoryExceedsBudget {
                system_tokens,
                tool_schema_tokens,
                history_tokens,
                total_tokens,
            } => write!(
                f,
                "prompt input needs {} estimated tokens (system {system_tokens}, schemas {tool_schema_tokens}, history {history_tokens}) but the total budget is {total_tokens}",
                system_tokens + tool_schema_tokens + history_tokens
            ),
            Self::SystemRoleInHistory => {
                write!(f, "conversation history must not contain a system message")
            }
        }
    }
}

impl Error for PromptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Context(source) => Some(source),
            Self::InvalidEnvironment { .. }
            | Self::InvalidProductDocumentation { .. }
            | Self::DuplicateSourceId { .. }
            | Self::RequiredLayersExceedBudget { .. }
            | Self::HistoryExceedsBudget { .. }
            | Self::SystemRoleInHistory => None,
        }
    }
}

#[cfg(test)]
mod tests;
