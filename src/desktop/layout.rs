//! Versioned, inert Workbench layout state owned by Xana rather than GPUI.

use super::{DesktopError, DesktopErrorCode};
use crate::{bounded_file, paths::XanaPaths};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write as _,
    path::{Path, PathBuf},
};

const LAYOUT_VERSION: u16 = 1;
const MAX_LAYOUT_BYTES: usize = 128 * 1024;
const MAX_LAYOUT_DEPTH: usize = 8;
const MAX_LAYOUT_NODES: usize = 31;
const MAX_PANEL_OCCURRENCES: usize = 16;
const MIN_SPLIT_PERMILLE: u16 = 100;
const MAX_SPLIT_PERMILLE: u16 = 900;

/// Trusted built-in Workbench panels. Unknown imported IDs become a harmless placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopPanelId {
    Conversation,
    Message,
    Activity,
    Summary,
    Artifacts,
    Usage,
    WorkingSet,
    #[serde(other)]
    Unavailable,
}

impl DesktopPanelId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Message => "Message",
            Self::Activity => "Activity",
            Self::Summary => "Summary",
            Self::Artifacts => "Artifacts",
            Self::Usage => "Usage",
            Self::WorkingSet => "Working Set",
            Self::Unavailable => "Unavailable panel",
        }
    }
}

/// Direction of one binary Workbench split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSplitAxis {
    Horizontal,
    Vertical,
}

/// Pointer/keyboard docking destination relative to one stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopDockPlacement {
    Tab,
    Left,
    Right,
    Above,
    Below,
}

/// One node in Xana's bounded binary split tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopLayoutNode {
    Split {
        id: String,
        axis: DesktopSplitAxis,
        ratio_permille: u16,
        first: Box<DesktopLayoutNode>,
        second: Box<DesktopLayoutNode>,
    },
    Stack {
        id: String,
        panels: Vec<DesktopPanelId>,
        active: usize,
    },
}

/// Validated Workbench layout persisted without paths, content, or commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopWorkbenchLayout {
    version: u16,
    root: DesktopLayoutNode,
    maximized: Option<DesktopPanelId>,
}

impl DesktopWorkbenchLayout {
    /// Xana's non-persisted fallback layout.
    pub fn recovery() -> Self {
        Self {
            version: LAYOUT_VERSION,
            root: DesktopLayoutNode::Split {
                id: "split-root".to_owned(),
                axis: DesktopSplitAxis::Horizontal,
                ratio_permille: 720,
                first: Box::new(DesktopLayoutNode::Split {
                    id: "split-primary".to_owned(),
                    axis: DesktopSplitAxis::Vertical,
                    ratio_permille: 760,
                    first: Box::new(DesktopLayoutNode::Stack {
                        id: "stack-conversation".to_owned(),
                        panels: vec![DesktopPanelId::Conversation],
                        active: 0,
                    }),
                    second: Box::new(DesktopLayoutNode::Stack {
                        id: "stack-message".to_owned(),
                        panels: vec![DesktopPanelId::Message],
                        active: 0,
                    }),
                }),
                second: Box::new(DesktopLayoutNode::Stack {
                    id: "stack-activity".to_owned(),
                    panels: vec![DesktopPanelId::Activity],
                    active: 0,
                }),
            },
            maximized: None,
        }
    }

    pub fn root(&self) -> &DesktopLayoutNode {
        &self.root
    }

    pub fn maximized(&self) -> Option<DesktopPanelId> {
        self.maximized
    }

    pub fn panels(&self) -> Vec<DesktopPanelId> {
        let mut panels = Vec::new();
        collect_panels(&self.root, &mut panels);
        panels
    }

    pub fn validate(&self) -> Result<(), DesktopError> {
        if self.version != LAYOUT_VERSION {
            return Err(layout_error(format!(
                "unsupported Workbench layout version {}",
                self.version
            )));
        }
        let mut facts = LayoutFacts::default();
        validate_node(&self.root, 1, &mut facts)?;
        if facts.nodes > MAX_LAYOUT_NODES {
            return Err(layout_error("Workbench layout contains too many nodes"));
        }
        if facts.panels > MAX_PANEL_OCCURRENCES {
            return Err(layout_error("Workbench layout contains too many panels"));
        }
        if let Some(maximized) = self.maximized
            && !self.panels().contains(&maximized)
        {
            return Err(layout_error(
                "maximized Workbench panel is not present in the layout",
            ));
        }
        Ok(())
    }

    /// Encodes only the inert, validated layout contract.
    pub fn to_inert_toml(&self) -> Result<String, DesktopError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(layout_error)
    }

    /// Decodes a bounded inert layout without accepting executable state.
    pub fn from_inert_toml(input: &[u8]) -> Result<Self, DesktopError> {
        if input.len() > MAX_LAYOUT_BYTES {
            return Err(layout_error("Workbench layout import exceeds 128 KiB"));
        }
        let layout: Self = toml::from_slice(input).map_err(layout_error)?;
        layout.validate()?;
        Ok(layout)
    }

    /// Reads one explicitly selected, non-symbolic-link TOML layout file.
    pub fn read_inert_file(path: &Path) -> Result<Self, DesktopError> {
        validate_layout_file_path(path, true)?;
        let bytes = bounded_file::read(path, MAX_LAYOUT_BYTES).map_err(layout_error)?;
        Self::from_inert_toml(&bytes)
    }

    /// Atomically writes one explicitly selected inert TOML layout file.
    pub fn write_inert_file(&self, path: &Path) -> Result<(), DesktopError> {
        validate_layout_file_path(path, false)?;
        let rendered = self.to_inert_toml()?;
        let mut file = atomic_write_file::AtomicWriteFile::open(path).map_err(layout_error)?;
        file.write_all(rendered.as_bytes())
            .and_then(|()| file.commit())
            .map_err(layout_error)
    }

    pub fn resize_split(&mut self, id: &str, ratio_permille: u16) -> Result<(), DesktopError> {
        if !(MIN_SPLIT_PERMILLE..=MAX_SPLIT_PERMILLE).contains(&ratio_permille) {
            return Err(layout_error("Workbench split ratio is outside safe bounds"));
        }
        let Some(split) = find_split_mut(&mut self.root, id) else {
            return Err(layout_error(format!("unknown Workbench split {id}")));
        };
        *split = ratio_permille;
        self.validate()
    }

    pub fn maximize(&mut self, panel: DesktopPanelId) -> Result<(), DesktopError> {
        if !self.panels().contains(&panel) {
            return Err(layout_error("cannot maximize a closed Workbench panel"));
        }
        self.maximized = Some(panel);
        Ok(())
    }

    pub fn restore(&mut self) {
        self.maximized = None;
    }

    pub fn activate_panel(&mut self, panel: DesktopPanelId) -> Result<(), DesktopError> {
        if activate_panel(&mut self.root, panel) {
            Ok(())
        } else {
            Err(layout_error("cannot activate a closed Workbench panel"))
        }
    }

    pub fn close_panel(&mut self, panel: DesktopPanelId) -> Result<(), DesktopError> {
        if panel == DesktopPanelId::Message {
            return Err(layout_error(
                "the Message panel remains reachable; move or maximize it instead",
            ));
        }
        let root = remove_panel(self.root.clone(), panel)
            .ok_or_else(|| layout_error("a Workbench layout cannot be empty"))?;
        let candidate = Self {
            version: self.version,
            root,
            maximized: (self.maximized != Some(panel))
                .then_some(self.maximized)
                .flatten(),
        };
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn reopen_panel(&mut self, panel: DesktopPanelId) -> Result<(), DesktopError> {
        if self.panels().contains(&panel) {
            return Ok(());
        }
        let mut candidate = self.clone();
        let Some((panels, active)) = first_stack_mut(&mut candidate.root) else {
            return Err(layout_error("Workbench layout has no panel stack"));
        };
        panels.push(panel);
        *active = panels.len().saturating_sub(1);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn dock_panel(
        &mut self,
        panel: DesktopPanelId,
        target_stack: &str,
        placement: DesktopDockPlacement,
    ) -> Result<(), DesktopError> {
        let without = remove_panel(self.root.clone(), panel)
            .ok_or_else(|| layout_error("cannot dock the only remaining Workbench panel"))?;
        let mut candidate = Self {
            version: self.version,
            root: without,
            maximized: self.maximized,
        };
        let next = next_node_number(&candidate.root);
        dock_into(&mut candidate.root, panel, target_stack, placement, next)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Moves a panel into a deterministic root-level split, or tabs it into
    /// the first remaining stack. This is the bounded keyboard/pointer adapter
    /// used by the initial Workbench instead of persisting framework state.
    pub fn dock_panel_at_root(
        &mut self,
        panel: DesktopPanelId,
        placement: DesktopDockPlacement,
    ) -> Result<(), DesktopError> {
        let without = remove_panel(self.root.clone(), panel)
            .ok_or_else(|| layout_error("cannot dock the only remaining Workbench panel"))?;
        let mut candidate = Self {
            version: self.version,
            root: without,
            maximized: None,
        };
        let next = next_node_number(&candidate.root);
        if placement == DesktopDockPlacement::Tab {
            let target = first_stack_id(&candidate.root)
                .ok_or_else(|| layout_error("Workbench layout has no panel stack"))?
                .to_owned();
            dock_into(
                &mut candidate.root,
                panel,
                &target,
                DesktopDockPlacement::Tab,
                next,
            )?;
        } else {
            let old = candidate.root;
            let fresh = DesktopLayoutNode::Stack {
                id: format!("stack-{next}"),
                panels: vec![panel],
                active: 0,
            };
            let (axis, first, second) = match placement {
                DesktopDockPlacement::Left => (DesktopSplitAxis::Horizontal, fresh, old),
                DesktopDockPlacement::Right => (DesktopSplitAxis::Horizontal, old, fresh),
                DesktopDockPlacement::Above => (DesktopSplitAxis::Vertical, fresh, old),
                DesktopDockPlacement::Below => (DesktopSplitAxis::Vertical, old, fresh),
                DesktopDockPlacement::Tab => unreachable!("handled above"),
            };
            candidate.root = DesktopLayoutNode::Split {
                id: format!("split-{next}"),
                axis,
                ratio_permille: 500,
                first: Box::new(first),
                second: Box::new(second),
            };
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

/// Source selected by the explicit layout-precedence chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLayoutSource {
    Conversation,
    UserDefault,
    Recovery,
}

/// Valid layout plus a bounded recovery explanation when persisted state failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResolvedLayout {
    pub layout: DesktopWorkbenchLayout,
    pub source: DesktopLayoutSource,
    pub warning: Option<String>,
}

pub(super) struct DesktopLayoutStore {
    root: PathBuf,
}

impl DesktopLayoutStore {
    pub(super) fn open(paths: &XanaPaths) -> Self {
        Self {
            root: paths.data_dir().join("frontend").join("workbench"),
        }
    }

    pub(super) fn resolve(&self, conversation: &str) -> DesktopResolvedLayout {
        let conversation_file = self.conversation_file(conversation);
        match self.read(&conversation_file) {
            Ok(Some(layout)) => DesktopResolvedLayout {
                layout,
                source: DesktopLayoutSource::Conversation,
                warning: None,
            },
            Err(error) => self.resolve_default(Some(error.message)),
            Ok(None) => self.resolve_default(None),
        }
    }

    pub(super) fn save_conversation(
        &self,
        conversation: &str,
        layout: &DesktopWorkbenchLayout,
    ) -> Result<(), DesktopError> {
        self.write(&self.conversation_file(conversation), layout)
    }

    pub(super) fn save_default(&self, layout: &DesktopWorkbenchLayout) -> Result<(), DesktopError> {
        self.write(&self.root.join("default.toml"), layout)
    }

    pub(super) fn clear_conversation(&self, conversation: &str) -> Result<(), DesktopError> {
        remove_optional(&self.conversation_file(conversation))
    }

    pub(super) fn clear_default(&self) -> Result<(), DesktopError> {
        remove_optional(&self.root.join("default.toml"))
    }

    pub(super) fn export(layout: &DesktopWorkbenchLayout) -> Result<String, DesktopError> {
        layout.to_inert_toml()
    }

    pub(super) fn import(input: &[u8]) -> Result<DesktopWorkbenchLayout, DesktopError> {
        DesktopWorkbenchLayout::from_inert_toml(input)
    }

    pub(super) fn resolve_default(&self, warning: Option<String>) -> DesktopResolvedLayout {
        match self.read(&self.root.join("default.toml")) {
            Ok(Some(layout)) => DesktopResolvedLayout {
                layout,
                source: DesktopLayoutSource::UserDefault,
                warning,
            },
            Ok(None) => DesktopResolvedLayout {
                layout: DesktopWorkbenchLayout::recovery(),
                source: DesktopLayoutSource::Recovery,
                warning,
            },
            Err(error) => DesktopResolvedLayout {
                layout: DesktopWorkbenchLayout::recovery(),
                source: DesktopLayoutSource::Recovery,
                warning: Some(match warning {
                    Some(previous) => format!("{previous}; {}", error.message),
                    None => error.message,
                }),
            },
        }
    }

    fn read(&self, path: &std::path::Path) -> Result<Option<DesktopWorkbenchLayout>, DesktopError> {
        let bytes = match bounded_file::read(path, MAX_LAYOUT_BYTES) {
            Ok(bytes) => bytes,
            Err(bounded_file::BoundedReadError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(layout_error(error)),
        };
        Self::import(&bytes).map(Some)
    }

    fn write(
        &self,
        path: &std::path::Path,
        layout: &DesktopWorkbenchLayout,
    ) -> Result<(), DesktopError> {
        let rendered = Self::export(layout)?;
        let Some(parent) = path.parent() else {
            return Err(layout_error("Workbench layout path has no parent"));
        };
        std::fs::create_dir_all(parent).map_err(layout_error)?;
        let mut file = atomic_write_file::AtomicWriteFile::open(path).map_err(layout_error)?;
        file.write_all(rendered.as_bytes())
            .and_then(|()| file.commit())
            .map_err(layout_error)
    }

    fn conversation_file(&self, conversation: &str) -> PathBuf {
        let digest = blake3::hash(conversation.as_bytes()).to_hex();
        self.root
            .join("conversations")
            .join(format!("{digest}.toml"))
    }
}

#[derive(Default)]
struct LayoutFacts {
    ids: HashSet<String>,
    panels_seen: HashSet<DesktopPanelId>,
    nodes: usize,
    panels: usize,
}

fn validate_node(
    node: &DesktopLayoutNode,
    depth: usize,
    facts: &mut LayoutFacts,
) -> Result<(), DesktopError> {
    if depth > MAX_LAYOUT_DEPTH {
        return Err(layout_error("Workbench layout is nested too deeply"));
    }
    facts.nodes = facts.nodes.saturating_add(1);
    let id = match node {
        DesktopLayoutNode::Split {
            id,
            ratio_permille,
            first,
            second,
            ..
        } => {
            if !(MIN_SPLIT_PERMILLE..=MAX_SPLIT_PERMILLE).contains(ratio_permille) {
                return Err(layout_error("Workbench split ratio is outside safe bounds"));
            }
            validate_node(first, depth + 1, facts)?;
            validate_node(second, depth + 1, facts)?;
            id
        }
        DesktopLayoutNode::Stack { id, panels, active } => {
            if panels.is_empty() || panels.len() > MAX_PANEL_OCCURRENCES {
                return Err(layout_error("Workbench panel stack has an invalid size"));
            }
            if *active >= panels.len() {
                return Err(layout_error(
                    "Workbench panel stack has an invalid active tab",
                ));
            }
            for panel in panels {
                if !facts.panels_seen.insert(*panel) {
                    return Err(layout_error("Workbench panels must have unique identities"));
                }
            }
            facts.panels = facts.panels.saturating_add(panels.len());
            id
        }
    };
    if id.is_empty() || id.len() > 96 || id.chars().any(char::is_control) {
        return Err(layout_error("Workbench node ID is invalid"));
    }
    if !facts.ids.insert(id.clone()) {
        return Err(layout_error("Workbench node IDs must be unique"));
    }
    Ok(())
}

fn collect_panels(node: &DesktopLayoutNode, output: &mut Vec<DesktopPanelId>) {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            collect_panels(first, output);
            collect_panels(second, output);
        }
        DesktopLayoutNode::Stack { panels, .. } => output.extend(panels),
    }
}

fn find_split_mut<'a>(node: &'a mut DesktopLayoutNode, id: &str) -> Option<&'a mut u16> {
    match node {
        DesktopLayoutNode::Split {
            id: candidate,
            ratio_permille,
            first,
            second,
            ..
        } => {
            if candidate == id {
                Some(ratio_permille)
            } else {
                find_split_mut(first, id).or_else(|| find_split_mut(second, id))
            }
        }
        DesktopLayoutNode::Stack { .. } => None,
    }
}

fn first_stack_mut(node: &mut DesktopLayoutNode) -> Option<(&mut Vec<DesktopPanelId>, &mut usize)> {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            first_stack_mut(first).or_else(|| first_stack_mut(second))
        }
        DesktopLayoutNode::Stack { panels, active, .. } => Some((panels, active)),
    }
}

fn first_stack_id(node: &DesktopLayoutNode) -> Option<&str> {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            first_stack_id(first).or_else(|| first_stack_id(second))
        }
        DesktopLayoutNode::Stack { id, .. } => Some(id),
    }
}

fn activate_panel(node: &mut DesktopLayoutNode, panel: DesktopPanelId) -> bool {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            activate_panel(first, panel) || activate_panel(second, panel)
        }
        DesktopLayoutNode::Stack { panels, active, .. } => {
            let Some(index) = panels.iter().position(|candidate| *candidate == panel) else {
                return false;
            };
            *active = index;
            true
        }
    }
}

fn remove_panel(node: DesktopLayoutNode, panel: DesktopPanelId) -> Option<DesktopLayoutNode> {
    match node {
        DesktopLayoutNode::Split {
            id,
            axis,
            ratio_permille,
            first,
            second,
        } => match (remove_panel(*first, panel), remove_panel(*second, panel)) {
            (Some(first), Some(second)) => Some(DesktopLayoutNode::Split {
                id,
                axis,
                ratio_permille,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        },
        DesktopLayoutNode::Stack {
            id,
            mut panels,
            active,
        } => {
            panels.retain(|candidate| *candidate != panel);
            (!panels.is_empty()).then(|| DesktopLayoutNode::Stack {
                id,
                active: active.min(panels.len().saturating_sub(1)),
                panels,
            })
        }
    }
}

fn dock_into(
    node: &mut DesktopLayoutNode,
    panel: DesktopPanelId,
    target: &str,
    placement: DesktopDockPlacement,
    next: usize,
) -> Result<(), DesktopError> {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            if contains_stack(first, target) {
                dock_into(first, panel, target, placement, next)
            } else if contains_stack(second, target) {
                dock_into(second, panel, target, placement, next)
            } else {
                Err(layout_error(format!("unknown Workbench stack {target}")))
            }
        }
        DesktopLayoutNode::Stack { id, panels, active } if id == target => {
            if placement == DesktopDockPlacement::Tab {
                panels.push(panel);
                *active = panels.len().saturating_sub(1);
                return Ok(());
            }
            let old = node.clone();
            let fresh = DesktopLayoutNode::Stack {
                id: format!("stack-{next}"),
                panels: vec![panel],
                active: 0,
            };
            let (axis, first, second) = match placement {
                DesktopDockPlacement::Left => (DesktopSplitAxis::Horizontal, fresh, old),
                DesktopDockPlacement::Right => (DesktopSplitAxis::Horizontal, old, fresh),
                DesktopDockPlacement::Above => (DesktopSplitAxis::Vertical, fresh, old),
                DesktopDockPlacement::Below => (DesktopSplitAxis::Vertical, old, fresh),
                DesktopDockPlacement::Tab => unreachable!("handled above"),
            };
            *node = DesktopLayoutNode::Split {
                id: format!("split-{next}"),
                axis,
                ratio_permille: 500,
                first: Box::new(first),
                second: Box::new(second),
            };
            Ok(())
        }
        DesktopLayoutNode::Stack { .. } => {
            Err(layout_error(format!("unknown Workbench stack {target}")))
        }
    }
}

fn contains_stack(node: &DesktopLayoutNode, target: &str) -> bool {
    match node {
        DesktopLayoutNode::Split { first, second, .. } => {
            contains_stack(first, target) || contains_stack(second, target)
        }
        DesktopLayoutNode::Stack { id, .. } => id == target,
    }
}

fn next_node_number(node: &DesktopLayoutNode) -> usize {
    fn collect(node: &DesktopLayoutNode, ids: &mut HashSet<String>) {
        match node {
            DesktopLayoutNode::Split {
                id, first, second, ..
            } => {
                ids.insert(id.clone());
                collect(first, ids);
                collect(second, ids);
            }
            DesktopLayoutNode::Stack { id, .. } => {
                ids.insert(id.clone());
            }
        }
    }
    let mut ids = HashSet::new();
    collect(node, &mut ids);
    (1..=MAX_LAYOUT_NODES + 1)
        .find(|candidate| {
            !ids.contains(&format!("stack-{candidate}"))
                && !ids.contains(&format!("split-{candidate}"))
        })
        .unwrap_or(MAX_LAYOUT_NODES + 1)
}

fn remove_optional(path: &std::path::Path) -> Result<(), DesktopError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(layout_error(error)),
    }
}

fn validate_layout_file_path(path: &Path, must_exist: bool) -> Result<(), DesktopError> {
    if path.extension().and_then(|value| value.to_str()) != Some("toml") {
        return Err(layout_error("Workbench layout files must use .toml"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(layout_error(
                "Workbench layout file cannot be a symbolic link",
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(layout_error("Workbench layout path is not a regular file"));
        }
        Ok(_) => {}
        Err(error) if !must_exist && error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(layout_error(error)),
    }
    let parent = path
        .parent()
        .ok_or_else(|| layout_error("Workbench layout path has no parent"))?;
    let metadata = std::fs::metadata(parent).map_err(layout_error)?;
    if !metadata.is_dir() {
        return Err(layout_error("Workbench layout parent is not a directory"));
    }
    Ok(())
}

fn layout_error(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::StateInvalid,
        format!("invalid Desktop Workbench layout: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn fixture() -> (tempfile::TempDir, DesktopLayoutStore) {
        let directory = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        let store = DesktopLayoutStore::open(&paths);
        (directory, store)
    }

    #[test]
    fn recovery_layout_is_valid_and_keeps_the_composer_reachable() {
        let layout = DesktopWorkbenchLayout::recovery();
        layout.validate().unwrap();
        assert_eq!(
            layout.panels(),
            vec![
                DesktopPanelId::Conversation,
                DesktopPanelId::Message,
                DesktopPanelId::Activity
            ]
        );
        let mut layout = layout;
        assert!(layout.close_panel(DesktopPanelId::Message).is_err());
    }

    #[test]
    fn dock_close_reopen_maximize_and_resize_preserve_invariants() {
        let mut layout = DesktopWorkbenchLayout::recovery();
        layout.reopen_panel(DesktopPanelId::Usage).unwrap();
        layout
            .dock_panel(
                DesktopPanelId::Usage,
                "stack-activity",
                DesktopDockPlacement::Below,
            )
            .unwrap();
        layout.resize_split("split-root", 640).unwrap();
        layout.maximize(DesktopPanelId::Usage).unwrap();
        layout.restore();
        layout.close_panel(DesktopPanelId::Usage).unwrap();
        layout.validate().unwrap();
    }

    #[test]
    fn rejected_dock_is_transactional() {
        let mut layout = DesktopWorkbenchLayout::recovery();
        layout.reopen_panel(DesktopPanelId::Usage).unwrap();
        let before = layout.clone();

        assert!(
            layout
                .dock_panel(
                    DesktopPanelId::Usage,
                    "missing-stack",
                    DesktopDockPlacement::Right,
                )
                .is_err()
        );
        assert_eq!(layout, before);
    }

    #[test]
    fn root_docking_supports_each_bounded_placement() {
        for placement in [
            DesktopDockPlacement::Tab,
            DesktopDockPlacement::Left,
            DesktopDockPlacement::Right,
            DesktopDockPlacement::Above,
            DesktopDockPlacement::Below,
        ] {
            let mut layout = DesktopWorkbenchLayout::recovery();
            layout.reopen_panel(DesktopPanelId::Usage).unwrap();
            layout
                .dock_panel_at_root(DesktopPanelId::Usage, placement)
                .unwrap();
            assert_eq!(
                layout
                    .panels()
                    .into_iter()
                    .filter(|panel| *panel == DesktopPanelId::Usage)
                    .count(),
                1
            );
            layout.validate().unwrap();
        }
    }

    #[test]
    fn conversation_then_default_then_recovery_precedence_is_exact() {
        let (_directory, store) = fixture();
        assert_eq!(
            store.resolve("native/one").source,
            DesktopLayoutSource::Recovery
        );
        let mut default = DesktopWorkbenchLayout::recovery();
        default.reopen_panel(DesktopPanelId::Summary).unwrap();
        store.save_default(&default).unwrap();
        assert_eq!(
            store.resolve("native/one").source,
            DesktopLayoutSource::UserDefault
        );
        let mut conversation = default.clone();
        conversation.reopen_panel(DesktopPanelId::Usage).unwrap();
        store
            .save_conversation("native/one", &conversation)
            .unwrap();
        let resolved = store.resolve("native/one");
        assert_eq!(resolved.source, DesktopLayoutSource::Conversation);
        assert!(resolved.layout.panels().contains(&DesktopPanelId::Usage));
        store.clear_conversation("native/one").unwrap();
        store.clear_default().unwrap();
        assert_eq!(
            store.resolve("native/one").source,
            DesktopLayoutSource::Recovery
        );
    }

    #[test]
    fn corrupt_or_future_layout_recovers_without_blocking_startup() {
        let (_directory, store) = fixture();
        let path = store.conversation_file("native/corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"version = 999\nroot = 'wrong'").unwrap();
        let resolved = store.resolve("native/corrupt");
        assert_eq!(resolved.source, DesktopLayoutSource::Recovery);
        assert!(resolved.warning.is_some());
        resolved.layout.validate().unwrap();
    }

    #[test]
    fn imported_unknown_panel_becomes_inert_placeholder() {
        let source = DesktopLayoutStore::export(&DesktopWorkbenchLayout::recovery()).unwrap();
        let source = source.replacen(
            "panels = [\"conversation\"]",
            "panels = [\"future_plugin\"]",
            1,
        );
        let imported = DesktopLayoutStore::import(source.as_bytes()).unwrap();
        assert!(imported.panels().contains(&DesktopPanelId::Unavailable));
        assert!(!source.contains("workspace"));
    }

    #[test]
    fn import_rejects_oversized_and_impossible_trees() {
        assert!(DesktopLayoutStore::import(&vec![b'x'; MAX_LAYOUT_BYTES + 1]).is_err());
        let invalid = br#"
version = 1

[root]
kind = "stack"
id = "empty"
panels = []
active = 0
"#;
        assert!(DesktopLayoutStore::import(invalid).is_err());
    }

    #[test]
    fn explicit_layout_file_round_trip_is_atomic_and_extension_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared-layout.toml");
        let mut layout = DesktopWorkbenchLayout::recovery();
        layout.reopen_panel(DesktopPanelId::Summary).unwrap();

        layout.write_inert_file(&path).unwrap();
        assert_eq!(
            DesktopWorkbenchLayout::read_inert_file(&path).unwrap(),
            layout
        );
        assert!(
            layout
                .write_inert_file(&directory.path().join("layout.exe"))
                .is_err()
        );
    }
}
