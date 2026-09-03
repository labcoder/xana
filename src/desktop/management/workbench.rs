//! User-wide Workbench preference projection and reset commands.

use super::{DesktopControlPlane, DesktopEntityMutationReceipt, DesktopError};
use crate::desktop::{DesktopLayoutSource, layout::DesktopLayoutStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWorkbenchPreferenceSnapshot {
    pub version: u16,
    pub source: String,
    pub panels: Vec<String>,
    pub warning: Option<String>,
    pub precedence: String,
}

impl DesktopControlPlane {
    pub fn workbench_preference_snapshot(&self) -> DesktopWorkbenchPreferenceSnapshot {
        let resolved = DesktopLayoutStore::open(&self.paths).resolve_default(None);
        DesktopWorkbenchPreferenceSnapshot {
            version: 1,
            source: match resolved.source {
                DesktopLayoutSource::Conversation => "conversation",
                DesktopLayoutSource::UserDefault => "user_default",
                DesktopLayoutSource::Recovery => "built_in",
            }
            .to_owned(),
            panels: resolved
                .layout
                .panels()
                .into_iter()
                .map(|panel| panel.label().to_owned())
                .collect(),
            warning: resolved.warning,
            precedence: "Conversation last-used layout, then user-wide default, then Xana's built-in recovery layout. Projects do not add another preference layer.".to_owned(),
        }
    }

    pub fn restore_builtin_workbench_layout(
        &self,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        DesktopLayoutStore::open(&self.paths).clear_default()?;
        Ok(DesktopEntityMutationReceipt {
            semantic_code: "workbench.default.clear.completed.v1".to_owned(),
            subject: "user-wide Workbench default".to_owned(),
            effect: "restored built-in fallback".to_owned(),
            detail: "Per-Conversation last-used layouts were preserved.".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{DesktopPanelId, DesktopWorkbenchLayout};
    use tempfile::tempdir;

    #[test]
    fn reset_clears_only_user_default() {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        let store = DesktopLayoutStore::open(&control.paths);
        let mut layout = DesktopWorkbenchLayout::recovery();
        layout.reopen_panel(DesktopPanelId::Summary).unwrap();
        store.save_default(&layout).unwrap();
        assert_eq!(
            control.workbench_preference_snapshot().source,
            "user_default"
        );

        control.restore_builtin_workbench_layout().unwrap();
        let reset = control.workbench_preference_snapshot();
        assert_eq!(reset.source, "built_in");
        assert!(!reset.panels.iter().any(|panel| panel == "Summary"));
    }
}
