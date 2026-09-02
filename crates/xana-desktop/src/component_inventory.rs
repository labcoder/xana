//! Reviewed ownership boundary for Desktop UI primitives.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentSource {
    GpuiAi,
    GpuiComponent,
    XanaComposition,
    Deferred,
}

impl ComponentSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::GpuiAi => "gpui-ai",
            Self::GpuiComponent => "gpui-component",
            Self::XanaComposition => "Xana composition",
            Self::Deferred => "Deferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ComponentRecord {
    pub(crate) id: &'static str,
    pub(crate) component: &'static str,
    pub(crate) source: ComponentSource,
    pub(crate) ownership: &'static str,
}

pub(crate) const INVENTORY: &[ComponentRecord] = &[
    ComponentRecord {
        id: "chat",
        component: "Chat",
        source: ComponentSource::GpuiAi,
        ownership: "Retained transcript/composer entity; Xana supplies stable message snapshots and intent handling.",
    },
    ComponentRecord {
        id: "prompt-bar",
        component: "PromptBar",
        source: ComponentSource::GpuiAi,
        ownership: "Retained IME-capable editor; Xana owns drafts, attachments, models, and submission lifecycle.",
    },
    ComponentRecord {
        id: "streaming-text",
        component: "StreamingText",
        source: ComponentSource::GpuiAi,
        ownership: "Snapshot renderer; Xana owns deltas, revisions, citations, and terminal content.",
    },
    ComponentRecord {
        id: "thinking",
        component: "Thinking",
        source: ComponentSource::GpuiAi,
        ownership: "Controlled disclosure; Xana owns trace data, duration, lifecycle, and explicit open state.",
    },
    ComponentRecord {
        id: "tool-call",
        component: "ToolCall",
        source: ComponentSource::GpuiAi,
        ownership: "Controlled invocation card; Xana owns authority, execution, result, and approval state.",
    },
    ComponentRecord {
        id: "approval",
        component: "ApprovalCard",
        source: ComponentSource::GpuiAi,
        ownership: "Typed decision surface; Xana validates current scope and applies the decision.",
    },
    ComponentRecord {
        id: "attachments",
        component: "AttachmentStrip",
        source: ComponentSource::GpuiAi,
        ownership: "Snapshot preview; Xana owns bytes, validation, upload, opening, and removal.",
    },
    ComponentRecord {
        id: "queue",
        component: "MessageQueue",
        source: ComponentSource::GpuiAi,
        ownership: "Stable-ID snapshot; Xana owns queue ordering and every mutation.",
    },
    ComponentRecord {
        id: "threads",
        component: "ThreadList",
        source: ComponentSource::GpuiAi,
        ownership: "Retained virtual list; Xana owns durable Conversations and active selection.",
    },
    ComponentRecord {
        id: "sidebar",
        component: "SidebarNav",
        source: ComponentSource::GpuiAi,
        ownership: "Retained virtual tree; Xana owns routes, capability filtering, and selected destination.",
    },
    ComponentRecord {
        id: "commands",
        component: "CommandSearch",
        source: ComponentSource::GpuiAi,
        ownership: "Retained searchable palette; Xana supplies the shared command catalog and dispatches typed intent.",
    },
    ComponentRecord {
        id: "context",
        component: "ContextMeter",
        source: ComponentSource::GpuiAi,
        ownership: "Snapshot readout; Xana owns measured usage, freshness, provenance, and cost formatting.",
    },
    ComponentRecord {
        id: "ordinary-controls",
        component: "Button, menu, dialog, tooltip",
        source: ComponentSource::GpuiComponent,
        ownership: "Library owns interaction/focus mechanics; Xana supplies semantic labels and actions.",
    },
    ComponentRecord {
        id: "catalog-shell",
        component: "Catalog and semantic frames",
        source: ComponentSource::XanaComposition,
        ownership: "Review-only composition, product tokens, localized copy, and deterministic fixtures.",
    },
    ComponentRecord {
        id: "workbench-dock",
        component: "Persistent dock layout",
        source: ComponentSource::Deferred,
        ownership: "M4-16/M4-17 decide bounded Workbench composition and persistence.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn inventory_has_stable_unique_ids_and_explicit_ownership() {
        let mut ids = HashSet::new();
        for record in INVENTORY {
            assert!(ids.insert(record.id));
            assert!(!record.component.trim().is_empty());
            assert!(!record.ownership.trim().is_empty());
        }
    }

    #[test]
    fn required_ai_components_are_reused_instead_of_reimplemented() {
        for id in [
            "chat",
            "prompt-bar",
            "streaming-text",
            "thinking",
            "tool-call",
            "approval",
            "attachments",
            "queue",
            "threads",
            "sidebar",
            "commands",
            "context",
        ] {
            let record = INVENTORY
                .iter()
                .find(|record| record.id == id)
                .expect("required component is inventoried");
            assert_eq!(record.source, ComponentSource::GpuiAi);
        }
    }
}
