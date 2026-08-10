//! Bounded multiline composer editing and selection.

use std::ops::Range;

pub(super) const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MoveDirection {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Composer {
    pub(super) text: String,
    pub(super) cursor: usize,
    anchor: Option<usize>,
}

impl Composer {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            anchor: None,
        }
    }

    pub(super) fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub(super) fn insert(&mut self, value: &str) -> Result<(), String> {
        if value.len() > MAX_INPUT_BYTES {
            return Err("composer input reached the 1 MiB limit".to_owned());
        }
        let value = sanitize_input(value);
        let removed = self.selection().map_or(0, |range| range.len());
        if self
            .text
            .len()
            .saturating_sub(removed)
            .saturating_add(value.len())
            > MAX_INPUT_BYTES
        {
            return Err("composer input reached the 1 MiB limit".to_owned());
        }
        self.delete_selection();
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
        Ok(())
    }

    pub(super) fn move_cursor(&mut self, direction: MoveDirection, select: bool) {
        let old = self.cursor;
        self.cursor = match direction {
            MoveDirection::Left => previous_boundary(&self.text, self.cursor),
            MoveDirection::Right => next_boundary(&self.text, self.cursor),
            MoveDirection::Home => line_start(&self.text, self.cursor),
            MoveDirection::End => line_end(&self.text, self.cursor),
            MoveDirection::Up => vertical_move(&self.text, self.cursor, false),
            MoveDirection::Down => vertical_move(&self.text, self.cursor, true),
        };
        if select {
            self.anchor.get_or_insert(old);
        } else {
            self.anchor = None;
        }
    }

    pub(super) fn place_cursor(&mut self, line: usize, column: usize, select: bool) {
        let old = self.cursor;
        let mut line_start = 0usize;
        for _ in 0..line {
            let Some(offset) = self.text[line_start..].find('\n') else {
                line_start = self.text.len();
                break;
            };
            line_start += offset + 1;
        }
        let line_end = self.text[line_start..]
            .find('\n')
            .map_or(self.text.len(), |offset| line_start + offset);
        self.cursor = self.text[line_start..line_end]
            .char_indices()
            .nth(column)
            .map_or(line_end, |(offset, _)| line_start + offset);
        if select {
            self.anchor.get_or_insert(old);
        } else {
            self.anchor = None;
        }
    }

    pub(super) fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let previous = previous_boundary(&self.text, self.cursor);
        if previous != self.cursor {
            self.text.drain(previous..self.cursor);
            self.cursor = previous;
        }
    }

    pub(super) fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let next = next_boundary(&self.text, self.cursor);
        if next != self.cursor {
            self.text.drain(self.cursor..next);
        }
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.text.drain(range.clone());
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    pub(super) fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        self.anchor = None;
        std::mem::take(&mut self.text)
    }

    pub(super) fn replace(&mut self, value: String) {
        self.text = sanitize_input(&value);
        self.cursor = self.text.len();
        self.anchor = None;
    }
}

pub(super) fn sanitize_input(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(MAX_INPUT_BYTES));
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        let character = if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            '\n'
        } else {
            character
        };
        match character {
            '\n' => output.push('\n'),
            '\t' => output.push_str("    "),
            character if !character.is_control() => output.push(character),
            _ => {}
        }
        if output.len() >= MAX_INPUT_BYTES {
            break;
        }
    }
    while output.len() > MAX_INPUT_BYTES || !output.is_char_boundary(output.len()) {
        output.pop();
    }
    output
}

fn previous_boundary(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .char_indices()
        .nth(1)
        .map_or(value.len(), |(index, _)| cursor + index)
}

fn line_start(value: &str, cursor: usize) -> usize {
    value[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .find('\n')
        .map_or(value.len(), |index| cursor + index)
}

fn vertical_move(value: &str, cursor: usize, down: bool) -> usize {
    let start = line_start(value, cursor);
    let column = value[start..cursor].chars().count();
    let target = if down {
        let end = line_end(value, cursor);
        if end == value.len() {
            return cursor;
        }
        end + 1
    } else {
        if start == 0 {
            return cursor;
        }
        line_start(value, start - 1)
    };
    let target_end = line_end(value, target);
    value[target..target_end]
        .char_indices()
        .nth(column)
        .map_or(target_end, |(index, _)| target + index)
}
