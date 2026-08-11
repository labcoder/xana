//! Bounded terminal-input normalization before TUI command semantics.

use super::composer::MAX_INPUT_BYTES;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{Stream, StreamExt};
use std::{io, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};

const INPUT_CHANNEL_CAPACITY: usize = 64;
const FALLBACK_PASTE_GAP: Duration = Duration::from_millis(20);
const CONFIRMED_PASTE_GAP: Duration = Duration::from_millis(50);
const FALLBACK_PASTE_MIN_EVENTS: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TerminalInputEvent {
    Raw(Event),
    Text(String),
    Paste(String),
}

pub(super) struct TerminalInput {
    receiver: mpsc::Receiver<io::Result<TerminalInputEvent>>,
    task: JoinHandle<()>,
    pending: Option<io::Result<TerminalInputEvent>>,
}

impl TerminalInput {
    pub(super) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let task = tokio::spawn(read_terminal_input(EventStream::new(), sender));
        Self {
            receiver,
            task,
            pending: None,
        }
    }

    pub(super) async fn next(&mut self) -> Option<io::Result<TerminalInputEvent>> {
        let mut event = if let Some(event) = self.pending.take() {
            event
        } else {
            self.receiver.recv().await?
        };
        if !is_pointer_drag(&event) {
            return Some(event);
        }

        while let Ok(next) = self.receiver.try_recv() {
            if is_pointer_drag(&next) {
                event = next;
            } else {
                self.pending = Some(next);
                break;
            }
        }
        Some(event)
    }
}

fn is_pointer_drag(event: &io::Result<TerminalInputEvent>) -> bool {
    matches!(
        event,
        Ok(TerminalInputEvent::Raw(Event::Mouse(mouse)))
            if matches!(mouse.kind, crossterm::event::MouseEventKind::Drag(_))
    )
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_terminal_input<S>(mut events: S, sender: mpsc::Sender<io::Result<TerminalInputEvent>>)
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    let mut pending = None;
    loop {
        let event = if let Some(event) = pending.take() {
            event
        } else {
            match events.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
                None => return,
            }
        };

        let Event::Key(key) = event else {
            let normalized = match event {
                Event::Paste(text) => TerminalInputEvent::Paste(text),
                event => TerminalInputEvent::Raw(event),
            };
            if sender.send(Ok(normalized)).await.is_err() {
                return;
            }
            continue;
        };
        let Some(fragment) = fallback_fragment(key) else {
            if sender
                .send(Ok(TerminalInputEvent::Raw(Event::Key(key))))
                .await
                .is_err()
            {
                return;
            }
            continue;
        };

        let mut burst = KeyBurst::new(key, fragment);
        let mut terminal_error = None;
        let mut reached_eof = false;
        loop {
            let next = tokio::select! {
                biased;
                event = events.next() => Some(event),
                _ = sleep(burst.next_gap()) => None,
            };
            match next {
                Some(Some(Ok(Event::Key(next)))) => {
                    if let Some(fragment) = fallback_fragment(next) {
                        burst.push(fragment);
                    } else {
                        pending = Some(Event::Key(next));
                        break;
                    }
                }
                Some(Some(Ok(event))) => {
                    pending = Some(event);
                    break;
                }
                Some(Some(Err(error))) => {
                    terminal_error = Some(error);
                    break;
                }
                Some(None) => {
                    reached_eof = true;
                    break;
                }
                None => break,
            }
        }

        if sender.send(Ok(burst.finish())).await.is_err() {
            return;
        }
        if let Some(error) = terminal_error {
            let _ = sender.send(Err(error)).await;
            return;
        }
        if reached_eof {
            return;
        }
    }
}

fn fallback_fragment(key: KeyEvent) -> Option<char> {
    if key.kind != KeyEventKind::Press
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(character) => Some(character),
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => Some('\n'),
        KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => Some('\t'),
        _ => None,
    }
}

struct KeyBurst {
    first: KeyEvent,
    text: String,
    event_count: usize,
    contains_line_break: bool,
}

impl KeyBurst {
    fn new(first: KeyEvent, fragment: char) -> Self {
        let mut burst = Self {
            first,
            text: String::new(),
            event_count: 0,
            contains_line_break: false,
        };
        burst.push(fragment);
        burst
    }

    fn push(&mut self, fragment: char) {
        self.event_count = self.event_count.saturating_add(1);
        self.contains_line_break |= fragment == '\n';
        if self.text.len() <= MAX_INPUT_BYTES {
            self.text.push(fragment);
        }
    }

    fn next_gap(&self) -> Duration {
        if self.contains_line_break || self.event_count >= FALLBACK_PASTE_MIN_EVENTS {
            CONFIRMED_PASTE_GAP
        } else {
            FALLBACK_PASTE_GAP
        }
    }

    fn finish(self) -> TerminalInputEvent {
        if self.event_count == 1 && matches!(self.first.code, KeyCode::Enter) {
            TerminalInputEvent::Raw(Event::Key(self.first))
        } else if (self.contains_line_break && self.event_count > 1)
            || self.event_count >= FALLBACK_PASTE_MIN_EVENTS
        {
            TerminalInputEvent::Paste(self.text)
        } else {
            TerminalInputEvent::Text(self.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use futures::{SinkExt, stream};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn multiline_key_burst_becomes_one_confirmed_paste() {
        let mut burst = KeyBurst::new(key(KeyCode::Char('a')), 'a');
        burst.push('\n');
        burst.push('b');
        assert_eq!(burst.finish(), TerminalInputEvent::Paste("a\nb".to_owned()));
    }

    #[test]
    fn short_key_burst_is_inserted_in_one_update() {
        let mut burst = KeyBurst::new(key(KeyCode::Char('a')), 'a');
        burst.push('b');
        assert_eq!(burst.finish(), TerminalInputEvent::Text("ab".to_owned()));
    }

    #[test]
    fn one_enter_retains_submit_semantics() {
        assert_eq!(
            KeyBurst::new(key(KeyCode::Enter), '\n').finish(),
            TerminalInputEvent::Raw(Event::Key(key(KeyCode::Enter)))
        );
    }

    #[test]
    fn long_single_line_key_burst_is_treated_as_paste() {
        let mut burst = KeyBurst::new(key(KeyCode::Char('a')), 'a');
        for fragment in "bcdefgh".chars() {
            burst.push(fragment);
        }
        assert_eq!(
            burst.finish(),
            TerminalInputEvent::Paste("abcdefgh".to_owned())
        );
    }

    #[tokio::test]
    async fn fallback_multiline_stream_emits_one_paste_instead_of_submit_actions() {
        let events = stream::iter([
            Ok(Event::Key(key(KeyCode::Char('a')))),
            Ok(Event::Key(key(KeyCode::Enter))),
            Ok(Event::Key(key(KeyCode::Char('b')))),
        ]);
        let (sender, mut receiver) = mpsc::channel(INPUT_CHANNEL_CAPACITY);

        read_terminal_input(events, sender).await;

        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            TerminalInputEvent::Paste("a\nb".to_owned())
        );
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn paced_multiline_key_stream_cannot_submit_each_line() {
        let (mut event_sender, events) = futures::channel::mpsc::unbounded();
        let producer = tokio::spawn(async move {
            for event in [
                Event::Key(key(KeyCode::Char('a'))),
                Event::Key(key(KeyCode::Enter)),
                Event::Key(key(KeyCode::Char('b'))),
            ] {
                event_sender.send(Ok(event)).await.unwrap();
                tokio::time::sleep(Duration::from_millis(12)).await;
            }
        });
        let (sender, mut receiver) = mpsc::channel(INPUT_CHANNEL_CAPACITY);

        read_terminal_input(events, sender).await;
        producer.await.unwrap();

        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            TerminalInputEvent::Paste("a\nb".to_owned())
        );
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn queued_pointer_drags_sample_the_latest_position() {
        let (sender, receiver) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let task = tokio::spawn(async {});
        let mut input = TerminalInput {
            receiver,
            task,
            pending: None,
        };
        for column in [2, 8, 21] {
            sender
                .send(Ok(TerminalInputEvent::Raw(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column,
                    row: 4,
                    modifiers: KeyModifiers::NONE,
                }))))
                .await
                .unwrap();
        }

        let event = input.next().await.unwrap().unwrap();

        assert!(matches!(
            event,
            TerminalInputEvent::Raw(Event::Mouse(MouseEvent { column: 21, .. }))
        ));
    }
}
