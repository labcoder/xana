//! Persistent full-screen settings browser over the shared settings module.

mod state;
mod view;

use super::lifecycle::TerminalSession;
use crate::{
    paths::XanaPaths,
    presentation::ResolvedPresentation,
    settings::{SettingsError, SettingsManager, SettingsSection},
};
use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use state::{Action, Overlay, SettingsState, UpdateEffect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsRunOutcome {
    pub(crate) applied_transactions: usize,
    pub(crate) requires_new_conversation: bool,
}

pub(crate) fn run(
    paths: &XanaPaths,
    initial_section: Option<&str>,
    initial_search: Option<&str>,
    profile: ResolvedPresentation,
) -> Result<SettingsRunOutcome> {
    let initial_section = initial_section
        .map(|section| {
            SettingsSection::parse(section)
                .ok_or_else(|| SettingsError::UnknownSection(section.to_owned()))
        })
        .transpose()?;
    let manager = SettingsManager::new(paths);
    let draft = manager.begin()?;
    let mut state = SettingsState::new(draft, initial_section, initial_search)?;
    let mut terminal = TerminalSession::enter().context("could not enter the settings terminal")?;

    loop {
        terminal
            .terminal_mut()
            .draw(|frame| view::render(frame, &state, profile))
            .context("could not draw Xana settings")?;
        let event = event::read().context("could not read settings input")?;
        let actions = actions_for_event(event, &state);
        let mut exit = false;
        for action in actions {
            match state.update(action) {
                UpdateEffect::None => {}
                UpdateEffect::Apply => match manager.commit(state.draft(), false) {
                    Ok(receipt) => {
                        let replacement = manager
                            .begin()
                            .context("settings applied but the refreshed catalog could not load")?;
                        state.apply_succeeded(&receipt, replacement);
                    }
                    Err(error) => state.apply_failed(&error),
                },
                UpdateEffect::Discard => {
                    let replacement = manager.begin()?;
                    state.discard_succeeded(replacement);
                }
                UpdateEffect::Exit => {
                    exit = true;
                    break;
                }
            }
        }
        if exit {
            break;
        }
    }

    Ok(SettingsRunOutcome {
        applied_transactions: state.applied_transactions,
        requires_new_conversation: state.requires_new_conversation(),
    })
}

fn actions_for_event(event: Event, state: &SettingsState) -> Vec<Action> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            action_for_key(key, state).into_iter().collect()
        }
        Event::Paste(text) if accepts_text(state) => text
            .chars()
            .filter(|character| !character.is_control())
            .take(4096)
            .map(Action::Input)
            .collect(),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => vec![Action::Up, Action::Up, Action::Up],
            MouseEventKind::ScrollDown => vec![Action::Down, Action::Down, Action::Down],
            _ => Vec::new(),
        },
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => Vec::new(),
        Event::Key(_) => Vec::new(),
    }
}

fn action_for_key(key: KeyEvent, state: &SettingsState) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('q') => Some(Action::Back),
            KeyCode::Char('s') => Some(Action::Apply),
            KeyCode::Char('f') => Some(Action::Search),
            _ => None,
        };
    }
    match &state.overlay {
        Some(Overlay::Search | Overlay::Text { .. }) => match key.code {
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Enter => Some(Action::Activate),
            KeyCode::Backspace => Some(Action::Backspace),
            KeyCode::Char(character) if !character.is_control() => Some(Action::Input(character)),
            _ => None,
        },
        Some(Overlay::Choice { .. }) => match key.code {
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Enter => Some(Action::Activate),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            _ => None,
        },
        Some(Overlay::Review { .. }) => match key.code {
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Enter => Some(Action::Activate),
            KeyCode::Char('a') => Some(Action::Apply),
            _ => None,
        },
        Some(Overlay::ConfirmDiscard) => match key.code {
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Enter | KeyCode::Char('d') => Some(Action::Discard),
            _ => None,
        },
        Some(Overlay::Help) => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => Some(Action::Back),
            _ => None,
        },
        None => match key.code {
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Enter => Some(Action::Activate),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => Some(Action::PreviousSection),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => Some(Action::NextSection),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            KeyCode::Char('/') => Some(Action::Search),
            KeyCode::Char('?') | KeyCode::F(1) => Some(Action::Help),
            KeyCode::Char('a') => Some(Action::Apply),
            KeyCode::Char('d') => Some(Action::Discard),
            KeyCode::Char('r') => Some(Action::Reset),
            KeyCode::Char('u') => Some(Action::Revert),
            _ => None,
        },
    }
}

fn accepts_text(state: &SettingsState) -> bool {
    matches!(state.overlay, Some(Overlay::Search | Overlay::Text { .. }))
}

#[cfg(test)]
mod tests;
