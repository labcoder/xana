//! Pure navigation and staged-edit transitions for the settings workspace.

use crate::settings::{
    SettingChange, SettingEntry, SettingKind, SettingsDraft, SettingsError, SettingsReceipt,
    SettingsSection, SettingsSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusTone {
    Neutral,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatusLine {
    pub(super) tone: StatusTone,
    pub(super) message: String,
}

impl StatusLine {
    fn neutral(message: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Neutral,
            message: message.into(),
        }
    }

    fn success(message: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Success,
            message: message.into(),
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Warning,
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Error,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Overlay {
    Search,
    Choice {
        key: String,
        label: String,
        choices: Vec<String>,
        selected: usize,
    },
    Text {
        key: String,
        label: String,
        input: String,
        hint: &'static str,
    },
    Review {
        changes: Vec<SettingChange>,
    },
    Help,
    ConfirmDiscard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    PreviousSection,
    NextSection,
    Activate,
    Reset,
    Revert,
    Search,
    Apply,
    Discard,
    Help,
    Back,
    Input(char),
    Backspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateEffect {
    None,
    Apply,
    Discard,
    Exit,
}

pub(super) struct SettingsState {
    draft: SettingsDraft,
    pub(super) snapshot: SettingsSnapshot,
    pub(super) section_index: usize,
    pub(super) selected: usize,
    pub(super) search: String,
    pub(super) overlay: Option<Overlay>,
    pub(super) status: StatusLine,
    pub(super) applied_transactions: usize,
    requires_new_conversation: bool,
}

impl SettingsState {
    pub(super) fn new(
        draft: SettingsDraft,
        initial_section: Option<SettingsSection>,
        initial_search: Option<&str>,
    ) -> Result<Self, SettingsError> {
        let snapshot = draft.preview()?;
        let section_index = initial_section
            .and_then(|wanted| {
                SettingsSection::all()
                    .iter()
                    .position(|section| *section == wanted)
            })
            .unwrap_or(0);
        let mut state = Self {
            draft,
            snapshot,
            section_index,
            selected: 0,
            search: initial_search.unwrap_or_default().trim().to_owned(),
            overlay: None,
            status: StatusLine::neutral(
                "Browse safely. Nothing is written until you review and apply.",
            ),
            applied_transactions: 0,
            requires_new_conversation: false,
        };
        state.clamp_selection();
        Ok(state)
    }

    pub(super) fn section(&self) -> SettingsSection {
        SettingsSection::all()[self.section_index]
    }

    pub(super) fn visible_entries(&self) -> Vec<&SettingEntry> {
        let query = self.search.trim().to_ascii_lowercase();
        self.snapshot
            .entries
            .iter()
            .filter(|entry| {
                query.is_empty() && entry.section == self.section() || !query.is_empty()
            })
            .filter(|entry| {
                query.is_empty()
                    || entry.key.to_ascii_lowercase().contains(&query)
                    || entry.label.to_ascii_lowercase().contains(&query)
                    || entry.description.to_ascii_lowercase().contains(&query)
                    || entry.value.display.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    pub(super) fn selected_entry(&self) -> Option<&SettingEntry> {
        self.visible_entries().get(self.selected).copied()
    }

    pub(super) fn pending_count(&self) -> usize {
        self.draft.pending_count().unwrap_or_default()
    }

    pub(super) fn pending_changes(&self) -> Vec<SettingChange> {
        self.draft.pending_changes().unwrap_or_default()
    }

    pub(super) const fn requires_new_conversation(&self) -> bool {
        self.requires_new_conversation
    }

    pub(super) fn update(&mut self, action: Action) -> UpdateEffect {
        if self.overlay.is_some() {
            return self.update_overlay(action);
        }
        match action {
            Action::Up => self.move_selection(-1),
            Action::Down => self.move_selection(1),
            Action::PageUp => self.move_selection(-8),
            Action::PageDown => self.move_selection(8),
            Action::Home => self.selected = 0,
            Action::End => self.selected = self.visible_entries().len().saturating_sub(1),
            Action::PreviousSection => self.move_section(-1),
            Action::NextSection => self.move_section(1),
            Action::Activate => self.activate_selected(),
            Action::Reset => self.reset_selected(),
            Action::Revert => self.revert_selected(),
            Action::Search => self.overlay = Some(Overlay::Search),
            Action::Apply => self.open_review(),
            Action::Discard => self.request_discard(),
            Action::Help => self.overlay = Some(Overlay::Help),
            Action::Back => return self.back(),
            Action::Input(_) | Action::Backspace => {}
        }
        UpdateEffect::None
    }

    fn update_overlay(&mut self, action: Action) -> UpdateEffect {
        match action {
            Action::Back => {
                self.overlay = None;
                UpdateEffect::None
            }
            Action::Help => {
                self.overlay = Some(Overlay::Help);
                UpdateEffect::None
            }
            Action::Apply if matches!(self.overlay, Some(Overlay::Review { .. })) => {
                UpdateEffect::Apply
            }
            Action::Discard if matches!(self.overlay, Some(Overlay::ConfirmDiscard)) => {
                UpdateEffect::Discard
            }
            Action::Activate => self.confirm_overlay(),
            Action::Up => {
                self.move_overlay_choice(-1);
                UpdateEffect::None
            }
            Action::Down => {
                self.move_overlay_choice(1);
                UpdateEffect::None
            }
            Action::Home => {
                self.select_overlay_edge(false);
                UpdateEffect::None
            }
            Action::End => {
                self.select_overlay_edge(true);
                UpdateEffect::None
            }
            Action::Input(character) => {
                self.input_overlay(character);
                UpdateEffect::None
            }
            Action::Backspace => {
                self.backspace_overlay();
                UpdateEffect::None
            }
            Action::Search
            | Action::Reset
            | Action::Revert
            | Action::PageUp
            | Action::PageDown
            | Action::PreviousSection
            | Action::NextSection
            | Action::Apply
            | Action::Discard => UpdateEffect::None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let length = self.visible_entries().len();
        if length == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(length.saturating_sub(1));
    }

    fn move_section(&mut self, delta: isize) {
        self.search.clear();
        let length = SettingsSection::all().len();
        self.section_index = self
            .section_index
            .saturating_add_signed(delta)
            .min(length.saturating_sub(1));
        self.selected = 0;
    }

    fn activate_selected(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.status = StatusLine::warning("No setting is selected.");
            return;
        };
        if !entry.editable {
            self.status = entry.action.map_or_else(
                || StatusLine::warning("This row is informational."),
                |action| StatusLine::warning(format!("Managed separately · run `{action}`")),
            );
            return;
        }
        let choices = match entry.kind {
            SettingKind::Boolean => vec!["true".to_owned(), "false".to_owned()],
            SettingKind::Choice => entry.choices.clone(),
            _ => Vec::new(),
        };
        if !choices.is_empty() {
            let current = entry.value.raw.as_deref();
            let selected = choices
                .iter()
                .position(|choice| Some(choice.as_str()) == current)
                .unwrap_or(0);
            self.overlay = Some(Overlay::Choice {
                key: entry.key,
                label: entry.label,
                choices,
                selected,
            });
            return;
        }
        let hint = match entry.kind {
            SettingKind::Bytes => "Examples: 65536, 4 MiB, 32 MB",
            SettingKind::DurationDays => "Examples: 7, 30d, 90 days",
            SettingKind::Integer => "Enter a whole non-negative number",
            SettingKind::OptionalPath => "Enter an exact executable path; R resets to automatic",
            SettingKind::Boolean | SettingKind::Choice | SettingKind::ReadOnly => "",
        };
        self.overlay = Some(Overlay::Text {
            key: entry.key,
            label: entry.label,
            input: entry.value.raw.unwrap_or_default(),
            hint,
        });
    }

    fn reset_selected(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        match self.draft.reset(&entry.key) {
            Ok(()) => self.finish_stage(format!("{} reset to its default.", entry.label)),
            Err(error) => self.status = StatusLine::error(error.to_string()),
        }
    }

    fn revert_selected(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if self.draft.revert(&entry.key) {
            self.refresh_snapshot();
            self.status = StatusLine::neutral(format!("Reverted staged edit for {}.", entry.label));
        } else {
            self.status = StatusLine::neutral("That setting has no staged edit.");
        }
    }

    fn open_review(&mut self) {
        let changes = self.pending_changes();
        if changes.is_empty() {
            self.status = StatusLine::neutral("Nothing to apply yet.");
        } else {
            self.overlay = Some(Overlay::Review { changes });
        }
    }

    fn request_discard(&mut self) {
        if self.pending_count() == 0 {
            self.status = StatusLine::neutral("Nothing is staged.");
        } else {
            self.overlay = Some(Overlay::ConfirmDiscard);
        }
    }

    fn back(&mut self) -> UpdateEffect {
        if !self.search.is_empty() {
            self.search.clear();
            self.selected = 0;
            self.status = StatusLine::neutral("Search cleared.");
            UpdateEffect::None
        } else if self.pending_count() > 0 {
            self.overlay = Some(Overlay::ConfirmDiscard);
            UpdateEffect::None
        } else {
            UpdateEffect::Exit
        }
    }

    fn confirm_overlay(&mut self) -> UpdateEffect {
        let Some(overlay) = self.overlay.take() else {
            return UpdateEffect::None;
        };
        match overlay {
            Overlay::Choice {
                key,
                label,
                choices,
                selected,
            } => {
                let value = &choices[selected.min(choices.len().saturating_sub(1))];
                match self.draft.set(&key, value) {
                    Ok(()) => self.finish_stage(format!("{label} staged as {value}.")),
                    Err(error) => self.status = StatusLine::error(error.to_string()),
                }
                UpdateEffect::None
            }
            Overlay::Text {
                key, label, input, ..
            } => {
                match self.draft.set(&key, &input) {
                    Ok(()) => self.finish_stage(format!("{label} staged.")),
                    Err(error) => {
                        self.status = StatusLine::error(error.to_string());
                        self.overlay = Some(Overlay::Text {
                            key,
                            label,
                            input,
                            hint: "Fix the value or press Esc to cancel",
                        });
                    }
                }
                UpdateEffect::None
            }
            Overlay::Search | Overlay::Help => UpdateEffect::None,
            Overlay::Review { .. } => UpdateEffect::Apply,
            Overlay::ConfirmDiscard => UpdateEffect::Discard,
        }
    }

    fn move_overlay_choice(&mut self, delta: isize) {
        let Some(Overlay::Choice {
            choices, selected, ..
        }) = &mut self.overlay
        else {
            return;
        };
        *selected = selected
            .saturating_add_signed(delta)
            .min(choices.len().saturating_sub(1));
    }

    fn select_overlay_edge(&mut self, end: bool) {
        let Some(Overlay::Choice {
            choices, selected, ..
        }) = &mut self.overlay
        else {
            return;
        };
        *selected = if end {
            choices.len().saturating_sub(1)
        } else {
            0
        };
    }

    fn input_overlay(&mut self, character: char) {
        match &mut self.overlay {
            Some(Overlay::Search) => {
                if self.search.len() < 256 {
                    self.search.push(character);
                    self.selected = 0;
                }
            }
            Some(Overlay::Text { input, .. }) if input.len() < 4096 => input.push(character),
            _ => {}
        }
    }

    fn backspace_overlay(&mut self) {
        match &mut self.overlay {
            Some(Overlay::Search) => {
                self.search.pop();
                self.selected = 0;
            }
            Some(Overlay::Text { input, .. }) => {
                input.pop();
            }
            _ => {}
        }
    }

    fn finish_stage(&mut self, message: String) {
        self.refresh_snapshot();
        self.status = StatusLine::success(message);
    }

    fn refresh_snapshot(&mut self) {
        match self.draft.preview() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.clamp_selection();
            }
            Err(error) => self.status = StatusLine::error(error.to_string()),
        }
    }

    fn clamp_selection(&mut self) {
        self.selected = self
            .selected
            .min(self.visible_entries().len().saturating_sub(1));
    }

    pub(super) fn draft(&self) -> &SettingsDraft {
        &self.draft
    }

    pub(super) fn apply_succeeded(
        &mut self,
        receipt: &SettingsReceipt,
        replacement: SettingsDraft,
    ) {
        self.requires_new_conversation |= receipt.requires_new_conversation();
        self.applied_transactions += usize::from(!receipt.changes.is_empty());
        self.draft = replacement;
        self.snapshot = self
            .draft
            .preview()
            .expect("freshly loaded settings draft must preview");
        self.overlay = None;
        self.clamp_selection();
        self.status = if receipt.changes.is_empty() {
            StatusLine::neutral("Settings already matched; no files changed.")
        } else {
            StatusLine::success(format!(
                "Applied {} change{} safely.",
                receipt.changes.len(),
                if receipt.changes.len() == 1 { "" } else { "s" }
            ))
        };
    }

    pub(super) fn apply_failed(&mut self, error: &SettingsError) {
        self.overlay = None;
        self.status = StatusLine::error(error.to_string());
    }

    pub(super) fn discard_succeeded(&mut self, replacement: SettingsDraft) {
        self.draft = replacement;
        self.snapshot = self
            .draft
            .preview()
            .expect("freshly loaded settings draft must preview");
        self.overlay = None;
        self.clamp_selection();
        self.status =
            StatusLine::neutral("Staged changes discarded; durable files were untouched.");
    }
}
