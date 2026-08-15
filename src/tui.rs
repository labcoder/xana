//! Native full-screen terminal frontend over Xana's embedded client contract.
//!
//! Crossterm/Ratatui types stop at this module. The state/update layer consumes
//! provider-neutral snapshots and runtime events and emits runtime commands.

mod activity;
mod clipboard;
mod command;
mod composer;
mod effects;
mod input;
mod lifecycle;
mod rich_text;
mod runner;
mod session;
mod state;
mod view;

use crate::presentation::{ComposerPreset, PresentationPreferences, ResolvedPresentation};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use input::TerminalInputEvent;
use lifecycle::TerminalSession;
use state::{InputAction, MoveDirection, TuiState};
use std::{io, path::PathBuf};

pub(crate) use runner::{run_managed, run_native};

pub(crate) fn restore_terminal_best_effort() {
    lifecycle::restore_process_terminal_best_effort();
}

pub(crate) struct PreparedTui {
    terminal: TerminalSession,
    profile: ResolvedPresentation,
    preferences: PresentationPreferences,
    preferences_path: PathBuf,
    clipboard: clipboard::Clipboard,
}

impl PreparedTui {
    pub(crate) const fn profile(&self) -> ResolvedPresentation {
        self.profile
    }
}

pub(crate) fn prepare(
    profile: ResolvedPresentation,
    preferences: PresentationPreferences,
    preferences_path: PathBuf,
) -> io::Result<PreparedTui> {
    let mut terminal = TerminalSession::enter()?;
    let state = TuiState::starting(preferences.composer);
    terminal
        .terminal_mut()
        .draw(|frame| view::render(frame, &state, profile))?;
    Ok(PreparedTui {
        terminal,
        profile,
        preferences,
        preferences_path,
        clipboard: clipboard::Clipboard::default(),
    })
}

fn terminal_input_action(
    event: TerminalInputEvent,
    state: &TuiState,
    area: ratatui::layout::Rect,
) -> Option<InputAction> {
    match event {
        TerminalInputEvent::Raw(event) => input_action(event, state, area),
        TerminalInputEvent::Text(text) => Some(InputAction::Insert(text)),
        TerminalInputEvent::Paste(text) => Some(InputAction::Paste(text)),
    }
}

fn input_action(
    event: Event,
    state: &TuiState,
    area: ratatui::layout::Rect,
) -> Option<InputAction> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::Quit),
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::CopyOrInterrupt),
        Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::OpenPalette),
        Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(match state.composer_preset {
            ComposerPreset::Submit => InputAction::Newline,
            ComposerPreset::Newline => InputAction::Submit,
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.intersects(KeyModifiers::CONTROL) => Some(InputAction::Submit),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.intersects(KeyModifiers::SHIFT) => Some(InputAction::Newline),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            kind: KeyEventKind::Press,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::Confirm
        } else {
            match state.composer_preset {
                ComposerPreset::Submit => InputAction::Submit,
                ComposerPreset::Newline => InputAction::Newline,
            }
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            ..
        }) => Some(InputAction::Cancel),
        Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::PaletteUp
        } else {
            InputAction::Move {
                direction: MoveDirection::Up,
                select: modifiers.contains(KeyModifiers::SHIFT),
            }
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::PaletteDown
        } else {
            InputAction::Move {
                direction: MoveDirection::Down,
                select: modifiers.contains(KeyModifiers::SHIFT),
            }
        }),
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) if matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
        ) =>
        {
            let direction = match code {
                KeyCode::Left => MoveDirection::Left,
                KeyCode::Right => MoveDirection::Right,
                KeyCode::Home => MoveDirection::Home,
                KeyCode::End => MoveDirection::End,
                _ => unreachable!(),
            };
            Some(InputAction::Move {
                direction,
                select: modifiers.contains(KeyModifiers::SHIFT),
            })
        }
        Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(InputAction::Backspace),
        Event::Key(KeyEvent {
            code: KeyCode::Delete,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(InputAction::Delete),
        Event::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Some(InputAction::Insert(character.to_string()))
        }
        Event::Paste(text) => Some(InputAction::Paste(text)),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => Some(InputAction::Scroll(-3)),
            MouseEventKind::ScrollDown => Some(InputAction::Scroll(3)),
            MouseEventKind::Down(MouseButton::Left) => {
                view::pointer_action(mouse.column, mouse.row, false, state, area)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                view::pointer_action(mouse.column, mouse.row, true, state, area)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                view::pointer_release_action(mouse.column, mouse.row, state, area)
            }
            _ => None,
        },
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Key(_) => None,
    }
}

#[cfg(test)]
mod tests;
