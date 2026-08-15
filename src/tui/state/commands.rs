//! Typed input, overlay, slash-command, and command-palette reduction.

use super::*;

impl TuiState {
    pub(in crate::tui) fn update_input(&mut self, action: InputAction) -> UpdateEffect {
        if self.overlay.is_some() {
            return self.update_overlay(action);
        }
        match action {
            InputAction::Insert(text) => {
                self.header_expanded = false;
                if let Err(reason) = self.composer.insert(&text) {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            InputAction::Paste(text) => {
                self.header_expanded = false;
                let text = bounded(sanitize_input(&text), MAX_INPUT_BYTES);
                if text.is_empty() {
                    self.status = "Paste contained no displayable text".to_owned();
                } else if let Some(path) = dropped_image_path(&text) {
                    return UpdateEffect::AttachDropped(path);
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
            InputAction::CopyOrInterrupt => {
                if let Some(text) = self
                    .conversation_selection
                    .as_ref()
                    .and_then(|selection| selection.text.clone())
                {
                    UpdateEffect::CopyText(text)
                } else {
                    self.interrupt()
                }
            }
            InputAction::Scroll(delta) => {
                self.conversation_selection = None;
                let maximum = self
                    .conversation_row_estimate()
                    .saturating_sub(1)
                    .min(usize::from(u16::MAX)) as u16;
                self.scroll = if delta.is_negative() {
                    self.scroll.saturating_add(delta.unsigned_abs())
                } else {
                    self.scroll.saturating_sub(delta as u16)
                }
                .min(maximum);
                if delta.is_negative() && self.history_has_older && self.scroll >= maximum {
                    UpdateEffect::LoadOlder(self.viewed_conversation.clone())
                } else {
                    UpdateEffect::None
                }
            }
            InputAction::BeginConversationSelection(start) => {
                self.conversation_selection = Some(ConversationSelection {
                    start,
                    end: start,
                    dragged: false,
                    text: None,
                });
                UpdateEffect::None
            }
            InputAction::ExtendConversationSelection(end) => {
                if let Some(selection) = self.conversation_selection.as_mut() {
                    selection.end = end;
                    selection.dragged |= end != selection.start;
                    selection.text = None;
                }
                UpdateEffect::None
            }
            InputAction::FinishConversationSelection { end, text } => {
                if let Some(selection) = self.conversation_selection.as_mut() {
                    selection.end = end;
                    selection.dragged |= end != selection.start;
                    selection.text = text
                        .filter(|text| !text.is_empty())
                        .map(|text| bounded(text, MAX_MESSAGE_BYTES));
                    if selection.text.is_none() {
                        self.conversation_selection = None;
                    }
                }
                UpdateEffect::None
            }
            InputAction::ClearConversationSelection => {
                self.conversation_selection = None;
                UpdateEffect::None
            }
            InputAction::PlaceCursor {
                line,
                column,
                width,
                scroll,
                select,
            } => {
                self.conversation_selection = None;
                self.composer
                    .place_visual_cursor(line, column, width, scroll, select);
                UpdateEffect::None
            }
            InputAction::ViewSession(conversation) => {
                self.conversation_selection = None;
                UpdateEffect::ViewSession(conversation)
            }
            InputAction::ToggleActivity(index) => {
                self.conversation_selection = None;
                if let Some(card) = self.activity.get_mut(index) {
                    card.expanded = !card.expanded;
                }
                UpdateEffect::None
            }
            InputAction::OpenActivityDetail(index) => {
                self.conversation_selection = None;
                if let Some(card) = self.activity.get(index)
                    && !card.detail.is_empty()
                {
                    self.overlay = Some(Overlay::ActivityDetail {
                        card: Box::new(card.clone()),
                        scroll: 0,
                        selection: None,
                    });
                }
                UpdateEffect::None
            }
            InputAction::ToggleSessionsView => {
                self.conversation_selection = None;
                self.rail_expanded = !self.rail_expanded;
                self.status = if self.rail_expanded {
                    "Sessions panel shown".to_owned()
                } else {
                    "Sessions panel hidden; use /sessions view show to restore it".to_owned()
                };
                UpdateEffect::PersistRail(self.rail_expanded)
            }
            InputAction::ToggleHeader => {
                self.conversation_selection = None;
                self.header_expanded = !self.header_expanded;
                self.status = if self.header_expanded {
                    "Xana header expanded".to_owned()
                } else {
                    "Xana header collapsed".to_owned()
                };
                UpdateEffect::None
            }
            InputAction::Cancel => {
                self.conversation_selection = None;
                self.composer.clear_selection();
                UpdateEffect::None
            }
            InputAction::Quit => UpdateEffect::Quit,
            InputAction::PaletteUp
            | InputAction::PaletteDown
            | InputAction::Confirm
            | InputAction::ChooseOverlay(_)
            | InputAction::BeginActivitySelection(_)
            | InputAction::ExtendActivitySelection(_)
            | InputAction::FinishActivitySelection { .. }
            | InputAction::ClearActivitySelection => UpdateEffect::None,
        }
    }

    fn update_overlay(&mut self, action: InputAction) -> UpdateEffect {
        match action {
            InputAction::Cancel => {
                if let Some(Overlay::ExternalImageApproval { input, .. }) = self.overlay.take() {
                    self.composer.replace(input);
                    self.status = "External image was not read; draft restored".to_owned();
                } else if let Some(Overlay::VisionApproval {
                    input,
                    images,
                    plan,
                    ..
                }) = self.overlay.take()
                {
                    self.composer.replace(input);
                    self.restore_images(images);
                    self.pending_vision_route = Some(plan.route.name);
                    self.status = "Vision specialist was not authorized; draft restored".to_owned();
                } else {
                    self.overlay = None;
                }
                UpdateEffect::None
            }
            InputAction::Insert(text) | InputAction::Paste(text) => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, selected })
                    | Some(Overlay::SessionPicker {
                        query, selected, ..
                    }) => {
                        append_bounded(query, &sanitize_input(&text), 256);
                        *selected = 0;
                    }
                    Some(Overlay::ProfileCreate {
                        fields,
                        selected,
                        error,
                    }) => {
                        append_bounded(&mut fields[*selected], &sanitize_input(&text), 256);
                        *error = None;
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
                    Some(Overlay::ProfileCreate {
                        fields,
                        selected,
                        error,
                    }) => {
                        fields[*selected].pop();
                        *error = None;
                    }
                    _ => {}
                }
                UpdateEffect::None
            }
            InputAction::PaletteUp => {
                if let Some(Overlay::ActivityDetail {
                    scroll, selection, ..
                }) = &mut self.overlay
                {
                    *selection = None;
                    *scroll = scroll.saturating_sub(1);
                } else if let Some(Overlay::CommandResult { scroll, .. }) = &mut self.overlay {
                    *scroll = scroll.saturating_sub(1);
                } else {
                    self.move_overlay_selection(false);
                }
                UpdateEffect::None
            }
            InputAction::PaletteDown => {
                if let Some(Overlay::ActivityDetail {
                    scroll, selection, ..
                }) = &mut self.overlay
                {
                    *selection = None;
                    *scroll = scroll.saturating_add(1);
                } else if let Some(Overlay::CommandResult { scroll, .. }) = &mut self.overlay {
                    *scroll = scroll.saturating_add(1);
                } else {
                    self.move_overlay_selection(true);
                }
                UpdateEffect::None
            }
            InputAction::Scroll(delta) => {
                if let Some(Overlay::ActivityDetail {
                    scroll, selection, ..
                }) = &mut self.overlay
                {
                    *selection = None;
                    *scroll = if delta.is_negative() {
                        scroll.saturating_sub(delta.unsigned_abs())
                    } else {
                        scroll.saturating_add(delta as u16)
                    };
                } else if let Some(Overlay::CommandResult { scroll, .. }) = &mut self.overlay {
                    *scroll = if delta.is_negative() {
                        scroll.saturating_sub(delta.unsigned_abs())
                    } else {
                        scroll.saturating_add(delta as u16)
                    };
                } else {
                    for _ in 0..delta.unsigned_abs() {
                        self.move_overlay_selection(delta.is_positive());
                    }
                }
                UpdateEffect::None
            }
            InputAction::Confirm | InputAction::Submit
                if matches!(
                    self.overlay,
                    Some(Overlay::ActivityDetail { .. } | Overlay::CommandResult { .. })
                ) =>
            {
                UpdateEffect::None
            }
            InputAction::Confirm | InputAction::Submit => self.confirm_overlay(),
            InputAction::CopyOrInterrupt => match &self.overlay {
                Some(Overlay::ActivityDetail { selection, .. }) => selection
                    .as_ref()
                    .and_then(|selection| selection.text.clone())
                    .map_or(UpdateEffect::None, UpdateEffect::CopyText),
                _ => UpdateEffect::None,
            },
            InputAction::BeginActivitySelection(start) => {
                if let Some(Overlay::ActivityDetail { selection, .. }) = &mut self.overlay {
                    *selection = Some(ConversationSelection {
                        start,
                        end: start,
                        dragged: false,
                        text: None,
                    });
                }
                UpdateEffect::None
            }
            InputAction::ExtendActivitySelection(end) => {
                if let Some(Overlay::ActivityDetail { selection, .. }) = &mut self.overlay
                    && let Some(selection) = selection
                {
                    selection.end = end;
                    selection.dragged |= end != selection.start;
                    selection.text = None;
                }
                UpdateEffect::None
            }
            InputAction::FinishActivitySelection { end, text } => {
                if let Some(Overlay::ActivityDetail { selection, .. }) = &mut self.overlay {
                    if let Some(active) = selection.as_mut() {
                        active.end = end;
                        active.dragged |= end != active.start;
                        active.text = text
                            .filter(|text| !text.is_empty())
                            .map(|text| bounded(text, MAX_ACTIVITY_BYTES));
                    }
                    if selection
                        .as_ref()
                        .is_some_and(|selection| selection.text.is_none())
                    {
                        *selection = None;
                    }
                }
                UpdateEffect::None
            }
            InputAction::ClearActivitySelection => {
                if let Some(Overlay::ActivityDetail { selection, .. }) = &mut self.overlay {
                    *selection = None;
                }
                UpdateEffect::None
            }
            InputAction::ChooseOverlay(index) => {
                self.conversation_selection = None;
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
            Some(Overlay::ExternalImageApproval { .. }) => 2,
            Some(Overlay::VisionApproval { .. }) => 4,
            Some(Overlay::Artifact { .. }) => 4,
            Some(Overlay::SessionPicker { query, choices, .. }) => choices
                .iter()
                .filter(|row| session_matches(row, query))
                .count(),
            Some(Overlay::ProfileCreate { .. }) => 3,
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
            | Some(Overlay::ExternalImageApproval { selected, .. })
            | Some(Overlay::VisionApproval { selected, .. })
            | Some(Overlay::Artifact { selected, .. })
            | Some(Overlay::SessionPicker { selected, .. })
            | Some(Overlay::ProfileCreate { selected, .. }) => *selected = index,
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
            Some(Overlay::ExternalImageApproval { selected, .. }) => (selected, 2),
            Some(Overlay::VisionApproval { selected, .. }) => (selected, 4),
            Some(Overlay::Artifact { selected, .. }) => (selected, 4),
            Some(Overlay::ProfileCreate { selected, .. }) => (selected, 3),
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
                        arguments: command.arguments.to_owned(),
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
            Overlay::ExternalImageApproval {
                operation_id,
                input,
                paths,
                selected,
                ..
            } => {
                if selected == 0 {
                    UpdateEffect::AttachAndSubmit {
                        operation_id,
                        input,
                        paths,
                        approved_external: true,
                    }
                } else {
                    self.composer.replace(input);
                    self.status = "External image was not read; draft restored".to_owned();
                    UpdateEffect::None
                }
            }
            Overlay::VisionApproval {
                operation_id,
                input,
                images,
                plan,
                selected,
            } => {
                let decision = [
                    crate::outbound::OutboundApprovalDecision::AllowOnce,
                    crate::outbound::OutboundApprovalDecision::SaveAllow,
                    crate::outbound::OutboundApprovalDecision::DenyOnce,
                    crate::outbound::OutboundApprovalDecision::SaveDeny,
                ]
                .get(selected)
                .copied();
                decision.map_or(UpdateEffect::None, |decision| UpdateEffect::PrepareVision {
                    operation_id,
                    input,
                    images,
                    plan,
                    decision,
                })
            }
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
            Overlay::ProfileCreate {
                fields,
                selected,
                error: _,
            } => {
                if selected < fields.len() - 1 {
                    self.overlay = Some(Overlay::ProfileCreate {
                        fields,
                        selected: selected + 1,
                        error: None,
                    });
                    return UpdateEffect::None;
                }
                if let Some((index, _)) = fields
                    .iter()
                    .enumerate()
                    .find(|(_, value)| value.trim().is_empty())
                {
                    self.overlay = Some(Overlay::ProfileCreate {
                        fields,
                        selected: index,
                        error: Some("Name, connection, and model are required".to_owned()),
                    });
                    return UpdateEffect::None;
                }
                let [name, connection, model] = fields.map(|value| {
                    shlex::try_quote(value.trim())
                        .map(|value| value.into_owned())
                        .unwrap_or(value)
                });
                UpdateEffect::ControlCommand {
                    family: "profile".to_owned(),
                    arguments: format!("create {name} --connection {connection} --model {model}"),
                }
            }
            Overlay::ActivityDetail { .. }
            | Overlay::CommandResult { .. }
            | Overlay::Help
            | Overlay::Queue => UpdateEffect::None,
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
        let paths = image_paths_in_text(&input);
        if paths.len() > MAX_IMAGES_PER_TURN {
            self.composer.replace(input);
            self.status = "At most 8 images may be attached to one turn".to_owned();
            return UpdateEffect::None;
        }
        if !paths.is_empty() {
            return UpdateEffect::AttachAndSubmit {
                operation_id: OperationId::new(),
                input,
                paths,
                approved_external: false,
            };
        }
        self.submit_text(input)
    }

    fn execute_command(&mut self, command: ParsedCommand, from_palette: bool) -> UpdateEffect {
        match command.id {
            CommandId::Help => {
                self.overlay = Some(Overlay::Help);
                UpdateEffect::None
            }
            CommandId::Header => {
                self.composer.take();
                self.header_expanded = match command.arguments.as_str() {
                    "" | "show" | "expanded" | "open" | "view show" => true,
                    "hide" | "collapsed" | "compact" | "view hide" => false,
                    _ => {
                        self.status = command_usage(CommandId::Header);
                        return UpdateEffect::None;
                    }
                };
                self.status = if self.header_expanded {
                    "Xana header expanded".to_owned()
                } else {
                    "Xana header collapsed".to_owned()
                };
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
            CommandId::Vision => {
                self.composer.take();
                if self.busy {
                    self.status =
                        "Wait for or interrupt the active turn before changing vision routing"
                            .to_owned();
                    UpdateEffect::None
                } else if command.arguments.is_empty() {
                    self.status = "Use /vision auto to prefer native input or /vision ROUTE to force one specialist on the next image turn".to_owned();
                    UpdateEffect::None
                } else if command.arguments == "auto" {
                    self.set_vision_route(None);
                    UpdateEffect::None
                } else {
                    self.set_vision_route(Some(command.arguments));
                    UpdateEffect::None
                }
            }
            CommandId::Sessions => {
                self.composer.take();
                let mut parts = command.arguments.split_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (None, None, None) => UpdateEffect::OpenSessionPicker,
                    (Some("view"), Some("show"), None) | (Some("expanded"), None, None) => {
                        self.rail_expanded = true;
                        self.status = "Sessions panel shown".to_owned();
                        UpdateEffect::PersistRail(true)
                    }
                    (Some("view"), Some("hide"), None) | (Some("collapsed"), None, None) => {
                        self.rail_expanded = false;
                        self.status =
                            "Sessions panel hidden; use /sessions view show to restore it"
                                .to_owned();
                        UpdateEffect::PersistRail(false)
                    }
                    (Some("archive"), selector, None) => self.archive_session(selector),
                    (Some("new"), None, None) => {
                        if self.busy {
                            self.status = "Wait for or interrupt the active turn before starting a new session".to_owned();
                            UpdateEffect::None
                        } else {
                            UpdateEffect::NewConversation
                        }
                    }
                    _ => {
                        self.status = command_usage(CommandId::Sessions);
                        UpdateEffect::None
                    }
                }
            }
            CommandId::Profile if command.arguments.trim() == "create" => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before creating a profile"
                        .to_owned();
                    UpdateEffect::None
                } else {
                    self.overlay = Some(Overlay::ProfileCreate {
                        fields: [String::new(), self.connection.clone(), self.model.clone()],
                        selected: 0,
                        error: None,
                    });
                    UpdateEffect::None
                }
            }
            CommandId::Project
            | CommandId::Profile
            | CommandId::Skill
            | CommandId::Plugin
            | CommandId::Mcp
            | CommandId::ExternalAgent
            | CommandId::Image => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before changing project/profile/skill/plugin state".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::ControlCommand {
                        family: match command.id {
                            CommandId::Project => "project",
                            CommandId::Profile => "profile",
                            CommandId::Skill => "skill",
                            CommandId::Plugin => "plugin",
                            CommandId::Mcp => "mcp",
                            CommandId::ExternalAgent => "external-agent",
                            CommandId::Image => "image",
                            _ => unreachable!(),
                        }
                        .to_owned(),
                        arguments: if command.arguments.is_empty() {
                            "list".to_owned()
                        } else {
                            command.arguments
                        },
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
            CommandId::Usage => {
                self.composer.take();
                let summary = self.managed_usage.map_or_else(
                    || self.native_usage.render(),
                    |(input, output, total)| {
                        format!(
                            "Current managed thread: input {input} · output {output} · total {total}. Provider quota, rate-limit reset, and wallet balance are not exposed by this managed runtime."
                        )
                    },
                );
                self.push_message(MessageKind::System, format!("Usage\n{summary}"));
                self.status = "Usage shown in the conversation".to_owned();
                UpdateEffect::None
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
                    "view hide" | "hidden" | "quiet" => ActivityVisibility::Hidden,
                    "view show" | "open" | "verbose" => ActivityVisibility::Open,
                    "view auto" | "auto" | "normal" => ActivityVisibility::Auto,
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
                if command.arguments == "--clipboard" {
                    UpdateEffect::AttachClipboard
                } else if command.arguments.is_empty() {
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

    fn archive_session(&mut self, selector: Option<&str>) -> UpdateEffect {
        let conversation = if let Some(selector) = selector {
            let matches = self
                .sessions
                .iter()
                .filter_map(|row| match &row.conversation {
                    ConversationRef::Managed {
                        connection,
                        thread_id,
                    } if selector == thread_id
                        || selector == format!("{connection}/{thread_id}") =>
                    {
                        Some(row.conversation.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [conversation] => conversation.clone(),
                [] => {
                    self.status = format!(
                        "No retained managed session matches {selector:?}; use /sessions to inspect exact IDs"
                    );
                    return UpdateEffect::None;
                }
                _ => {
                    self.status =
                        format!("Managed session ID {selector:?} is ambiguous; use CONNECTION/ID");
                    return UpdateEffect::None;
                }
            }
        } else {
            self.viewed_conversation.clone()
        };
        if conversation == self.runtime_conversation {
            self.status = "The active session cannot be archived; view an inactive managed session or pass its ID".to_owned();
            UpdateEffect::None
        } else if matches!(conversation, ConversationRef::Managed { .. }) {
            UpdateEffect::ArchiveConversation(conversation)
        } else {
            self.status =
                "Only retained managed sessions can be archived in this release".to_owned();
            UpdateEffect::None
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

fn dropped_image_path(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.contains(['\r', '\n']) {
        return None;
    }
    let candidate = if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let extension = std::path::Path::new(candidate)
        .extension()
        .and_then(std::ffi::OsStr::to_str)?;
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif"
    )
    .then(|| candidate.to_owned())
}

fn command_usage(id: CommandId) -> String {
    let usages = command::usages(id);
    if usages.is_empty() {
        "Invalid command".to_owned()
    } else {
        format!("Usage: {}", usages.join(" | "))
    }
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
