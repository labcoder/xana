//! Typed input, overlay, slash-command, and command-palette reduction.

use super::*;

impl TuiState {
    pub(in crate::tui) fn update_input(&mut self, action: InputAction) -> UpdateEffect {
        if self.overlay.is_some() {
            return self.update_overlay(action);
        }
        match action {
            InputAction::Insert(text) => {
                if let Err(reason) = self.composer.insert(&text) {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            InputAction::Paste(text) => {
                let text = bounded(sanitize_input(&text), MAX_INPUT_BYTES);
                if text.is_empty() {
                    self.status = "Paste contained no displayable text".to_owned();
                } else {
                    self.overlay = Some(Overlay::PastePreview { text });
                }
                UpdateEffect::None
            }
            InputAction::Move { direction, select } => {
                self.composer.move_cursor(direction, select);
                UpdateEffect::None
            }
            InputAction::Backspace => {
                self.composer.backspace();
                UpdateEffect::None
            }
            InputAction::Delete => {
                self.composer.delete();
                UpdateEffect::None
            }
            InputAction::Submit => self.submit_composer(),
            InputAction::Newline => {
                if let Err(reason) = self.composer.insert("\n") {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            InputAction::OpenPalette => {
                self.overlay = Some(Overlay::Palette {
                    query: String::new(),
                    selected: 0,
                });
                UpdateEffect::None
            }
            InputAction::Interrupt => self.interrupt(),
            InputAction::Scroll(delta) => {
                self.scroll = if delta.is_negative() {
                    self.scroll.saturating_add(delta.unsigned_abs())
                } else {
                    self.scroll.saturating_sub(delta as u16)
                };
                if delta.is_negative()
                    && self.history_has_older
                    && usize::from(self.scroll) >= self.messages.len().saturating_sub(1)
                {
                    UpdateEffect::LoadOlder(self.viewed_conversation.clone())
                } else {
                    UpdateEffect::None
                }
            }
            InputAction::PlaceCursor {
                line,
                column,
                select,
            } => {
                self.composer.place_cursor(line, column, select);
                UpdateEffect::None
            }
            InputAction::ViewSession(conversation) => UpdateEffect::ViewSession(conversation),
            InputAction::ToggleActivity(index) => {
                if let Some(card) = self.activity.get_mut(index) {
                    card.expanded = !card.expanded;
                }
                UpdateEffect::None
            }
            InputAction::ToggleRail => {
                self.rail_expanded = !self.rail_expanded;
                self.status = if self.rail_expanded {
                    "Wide session rail expanded".to_owned()
                } else {
                    "Wide session rail collapsed".to_owned()
                };
                UpdateEffect::PersistRail(self.rail_expanded)
            }
            InputAction::Cancel => {
                self.composer.clear_selection();
                UpdateEffect::None
            }
            InputAction::Quit => UpdateEffect::Quit,
            InputAction::PaletteUp
            | InputAction::PaletteDown
            | InputAction::Confirm
            | InputAction::ChooseOverlay(_) => UpdateEffect::None,
        }
    }

    fn update_overlay(&mut self, action: InputAction) -> UpdateEffect {
        match action {
            InputAction::Cancel => {
                self.overlay = None;
                UpdateEffect::None
            }
            InputAction::Insert(text) => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, selected })
                    | Some(Overlay::SessionPicker {
                        query, selected, ..
                    }) => {
                        append_bounded(query, &sanitize_input(&text), 256);
                        *selected = 0;
                    }
                    _ => {}
                }
                UpdateEffect::None
            }
            InputAction::Backspace => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, selected })
                    | Some(Overlay::SessionPicker {
                        query, selected, ..
                    }) => {
                        query.pop();
                        *selected = 0;
                    }
                    _ => {}
                }
                UpdateEffect::None
            }
            InputAction::PaletteUp => {
                self.move_overlay_selection(false);
                UpdateEffect::None
            }
            InputAction::PaletteDown => {
                self.move_overlay_selection(true);
                UpdateEffect::None
            }
            InputAction::Confirm | InputAction::Submit => self.confirm_overlay(),
            InputAction::ChooseOverlay(index) => {
                if self.select_overlay(index) {
                    self.confirm_overlay()
                } else {
                    UpdateEffect::None
                }
            }
            InputAction::Quit => UpdateEffect::Quit,
            _ => UpdateEffect::None,
        }
    }

    fn select_overlay(&mut self, index: usize) -> bool {
        let len = match &self.overlay {
            Some(Overlay::Palette { query, .. }) => command::search(query).len(),
            Some(Overlay::ModelPicker { choices, .. })
            | Some(Overlay::ReasoningPicker { choices, .. }) => choices.len(),
            Some(Overlay::Approval { prompt, .. }) => approval_choice_count(prompt),
            Some(Overlay::Artifact { .. }) => 4,
            Some(Overlay::SessionPicker { query, choices, .. }) => choices
                .iter()
                .filter(|row| session_matches(row, query))
                .count(),
            _ => 0,
        };
        if index >= len {
            return false;
        }
        match &mut self.overlay {
            Some(Overlay::Palette { selected, .. })
            | Some(Overlay::ModelPicker { selected, .. })
            | Some(Overlay::ReasoningPicker { selected, .. })
            | Some(Overlay::Approval { selected, .. })
            | Some(Overlay::Artifact { selected, .. })
            | Some(Overlay::SessionPicker { selected, .. }) => *selected = index,
            _ => return false,
        }
        true
    }

    fn move_overlay_selection(&mut self, down: bool) {
        let (selected, len) = match &mut self.overlay {
            Some(Overlay::Palette { query, selected }) => (selected, command::search(query).len()),
            Some(Overlay::ModelPicker { choices, selected })
            | Some(Overlay::ReasoningPicker { choices, selected }) => (selected, choices.len()),
            Some(Overlay::Approval { prompt, selected }) => {
                (selected, approval_choice_count(prompt))
            }
            Some(Overlay::Artifact { selected, .. }) => (selected, 4),
            Some(Overlay::SessionPicker {
                query,
                choices,
                selected,
            }) => (
                selected,
                choices
                    .iter()
                    .filter(|row| session_matches(row, query))
                    .count(),
            ),
            _ => return,
        };
        if len == 0 {
            *selected = 0;
        } else if down {
            *selected = (*selected + 1).min(len - 1);
        } else {
            *selected = selected.saturating_sub(1);
        }
    }

    fn confirm_overlay(&mut self) -> UpdateEffect {
        let Some(overlay) = self.overlay.take() else {
            return UpdateEffect::None;
        };
        match overlay {
            Overlay::PastePreview { text } => {
                if let Err(reason) = self.composer.insert(&text) {
                    self.status = reason;
                } else {
                    self.status = "Pasted text inserted as untrusted draft data".to_owned();
                }
                UpdateEffect::None
            }
            Overlay::Palette { query, selected } => {
                let Some(command) = command::search(&query).get(selected).copied() else {
                    return UpdateEffect::None;
                };
                self.execute_command(
                    ParsedCommand {
                        id: command.id,
                        arguments: String::new(),
                    },
                    true,
                )
            }
            Overlay::ModelPicker { choices, selected } => choices
                .get(selected)
                .cloned()
                .map_or(UpdateEffect::None, UpdateEffect::SelectModel),
            Overlay::ReasoningPicker { choices, selected } => choices
                .get(selected)
                .cloned()
                .map_or(UpdateEffect::None, UpdateEffect::SetReasoning),
            Overlay::SessionPicker {
                query,
                choices,
                selected,
            } => choices
                .into_iter()
                .filter(|row| session_matches(row, &query))
                .nth(selected)
                .map_or(UpdateEffect::None, |row| {
                    UpdateEffect::ViewSession(row.conversation)
                }),
            Overlay::Approval { prompt, selected } => self.confirm_approval(*prompt, selected),
            Overlay::Artifact {
                artifact, selected, ..
            } => {
                let action = [
                    ArtifactAction::Preview,
                    ArtifactAction::InsertReference,
                    ArtifactAction::Reveal,
                    ArtifactAction::Open,
                ]
                .get(selected)
                .copied();
                action.map_or(UpdateEffect::None, |action| UpdateEffect::ArtifactAction {
                    record: artifact.record.clone(),
                    action,
                })
            }
            Overlay::Help | Overlay::Queue => UpdateEffect::None,
        }
    }

    fn submit_composer(&mut self) -> UpdateEffect {
        let trimmed = self.composer.text.trim();
        if trimmed.starts_with('/') {
            return match command::parse(trimmed) {
                Ok(command) => self.execute_command(command, false),
                Err(reason) => {
                    self.status = reason;
                    UpdateEffect::None
                }
            };
        }
        let input = self.composer.take().trim().to_owned();
        self.submit_text(input)
    }

    fn execute_command(&mut self, command: ParsedCommand, from_palette: bool) -> UpdateEffect {
        match command.id {
            CommandId::Help => {
                self.overlay = Some(Overlay::Help);
                UpdateEffect::None
            }
            CommandId::Send => {
                let input = if command.arguments.is_empty() {
                    if from_palette {
                        self.composer.take().trim().to_owned()
                    } else {
                        self.status = "/send requires MESSAGE when used as slash input".to_owned();
                        return UpdateEffect::None;
                    }
                } else {
                    self.composer.take();
                    command.arguments
                };
                self.submit_text(input)
            }
            CommandId::Newline => {
                self.composer.take();
                if let Err(reason) = self.composer.insert("\n") {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            CommandId::Interrupt => {
                self.composer.take();
                self.interrupt()
            }
            CommandId::Steer => {
                self.composer.take();
                let Some(operation_id) = self.active_operation else {
                    self.status = "Steering requires an active turn".to_owned();
                    return UpdateEffect::None;
                };
                if !self.capabilities.steer {
                    self.status = "This execution owner does not support same-turn steering; submit a queued follow-up instead".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    self.status = command_usage(CommandId::Steer);
                    return UpdateEffect::None;
                }
                UpdateEffect::Steer {
                    operation_id,
                    input: command.arguments,
                }
            }
            CommandId::Model => {
                self.composer.take();
                if !self.capabilities.model {
                    self.status = "This execution owner cannot change models in place".to_owned();
                    return UpdateEffect::None;
                }
                if self.busy {
                    self.status =
                        "Wait for or interrupt the active turn before changing model".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    UpdateEffect::OpenModelPicker
                } else {
                    UpdateEffect::SelectModel(command.arguments)
                }
            }
            CommandId::Reasoning => {
                self.composer.take();
                if !self.capabilities.reasoning {
                    self.status = "Native Xana reasoning is selected by the model; this owner has no in-thread reasoning control".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    UpdateEffect::OpenReasoningPicker
                } else {
                    UpdateEffect::SetReasoning(command.arguments)
                }
            }
            CommandId::Sessions => {
                self.composer.take();
                match command.arguments.as_str() {
                    "" => UpdateEffect::OpenSessionPicker,
                    "expanded" => {
                        self.rail_expanded = true;
                        self.status = "Wide session rail expanded".to_owned();
                        UpdateEffect::PersistRail(true)
                    }
                    "collapsed" => {
                        self.rail_expanded = false;
                        self.status = "Wide session rail collapsed".to_owned();
                        UpdateEffect::PersistRail(false)
                    }
                    _ => {
                        self.status = command_usage(CommandId::Sessions);
                        UpdateEffect::None
                    }
                }
            }
            CommandId::Setup => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before setup".to_owned();
                    UpdateEffect::None
                } else {
                    match crate::setup::args_for_request(&command.arguments) {
                        Ok(_) => UpdateEffect::Setup(command.arguments),
                        Err(error) => {
                            self.status = error.to_string();
                            UpdateEffect::None
                        }
                    }
                }
            }
            CommandId::Doctor => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before doctor".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::Doctor
                }
            }
            CommandId::Reset => {
                self.composer.take();
                if !from_palette {
                    self.status =
                        "Reset is a guarded command-palette lifecycle action, not a slash command"
                            .to_owned();
                    UpdateEffect::None
                } else if self.busy {
                    self.status = "Wait for or interrupt the active turn before reset".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::Reset
                }
            }
            CommandId::Activity => {
                self.composer.take();
                self.activity_visibility = match command.arguments.as_str() {
                    "hidden" | "quiet" => ActivityVisibility::Hidden,
                    "open" | "verbose" => ActivityVisibility::Open,
                    "auto" | "normal" | "" => ActivityVisibility::Auto,
                    _ => {
                        self.status = command_usage(CommandId::Activity);
                        return UpdateEffect::None;
                    }
                };
                self.status = format!("Activity display: {:?}", self.activity_visibility);
                UpdateEffect::PersistActivity(self.activity_visibility.into())
            }
            CommandId::Artifact => {
                self.composer.take();
                let requested = command.arguments.trim();
                if requested.is_empty() {
                    self.status = command_usage(CommandId::Artifact);
                    return UpdateEffect::None;
                }
                let artifact = self.messages.iter().rev().find_map(|message| {
                    message
                        .document
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.record.reference.id.to_string() == requested)
                });
                if let Some(artifact) = artifact.cloned() {
                    self.overlay = Some(Overlay::Artifact {
                        artifact: Box::new(artifact),
                        selected: 0,
                        preview: None,
                    });
                } else {
                    self.status =
                        "Artifact is not visible in the bounded conversation view".to_owned();
                }
                UpdateEffect::None
            }
            CommandId::Attach => {
                self.composer.take();
                if command.arguments.is_empty() {
                    self.status = command_usage(CommandId::Attach);
                    UpdateEffect::None
                } else {
                    UpdateEffect::Attach(command.arguments)
                }
            }
            CommandId::Queue => {
                self.composer.take();
                self.queue_command(&command.arguments)
            }
            CommandId::Clear => {
                self.composer.take();
                if self.busy {
                    self.status = "Interrupt or finish the active turn before clearing".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::ClearConversation
                }
            }
            CommandId::Composer => {
                self.composer.take();
                let preset = match command.arguments.as_str() {
                    "submit" => ComposerPreset::Submit,
                    "newline" => ComposerPreset::Newline,
                    _ => {
                        self.status = command_usage(CommandId::Composer);
                        return UpdateEffect::None;
                    }
                };
                self.composer_preset = preset;
                self.status = format!("Composer preset: {preset:?}");
                UpdateEffect::PersistComposer(preset)
            }
            CommandId::Quit => UpdateEffect::Quit,
        }
    }

    fn queue_command(&mut self, arguments: &str) -> UpdateEffect {
        let mut parts = arguments.split_whitespace();
        match parts.next() {
            None => {
                self.overlay = Some(Overlay::Queue);
            }
            Some("remove") => match parse_queue_index(parts.next(), self.followups.len()) {
                Ok(index) => {
                    self.followups.remove(index);
                    self.status = format!("Removed queued follow-up {}", index + 1);
                }
                Err(reason) => self.status = reason,
            },
            Some("edit") => match parse_queue_index(parts.next(), self.followups.len()) {
                Ok(index) => {
                    if let Some(turn) = self.followups.remove(index) {
                        self.composer.replace(turn.input);
                        self.pending_images = turn.images;
                        self.status = format!("Editing queued follow-up {}", index + 1);
                    }
                }
                Err(reason) => self.status = reason,
            },
            Some(_) => self.status = command_usage(CommandId::Queue),
        }
        UpdateEffect::None
    }
}

fn command_usage(id: CommandId) -> String {
    command::COMMANDS
        .iter()
        .find(|command| command.id == id)
        .map_or_else(
            || "Invalid command".to_owned(),
            |command| format!("Usage: {}", command.usage),
        )
}

fn parse_queue_index(value: Option<&str>, len: usize) -> Result<usize, String> {
    let index = value
        .ok_or_else(|| command_usage(CommandId::Queue))?
        .parse::<usize>()
        .map_err(|_| command_usage(CommandId::Queue))?;
    if index == 0 || index > len {
        return Err(format!(
            "Queued follow-up index must be between 1 and {len}"
        ));
    }
    Ok(index - 1)
}
