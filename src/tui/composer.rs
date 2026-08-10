//! Bounded multiline composer editing and selection.

use std::ops::Range;
use unicode_width::UnicodeWidthChar;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComposerViewport {
    pub(super) rows: u16,
    pub(super) scroll: u16,
    pub(super) cursor_row: u16,
    pub(super) cursor_column: u16,
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

    pub(super) fn viewport(&self, width: u16, maximum_rows: u16) -> ComposerViewport {
        let width = usize::from(width.max(1));
        let maximum_rows = maximum_rows.max(1);
        let (cursor_row, cursor_column) = visual_position(&self.text, self.cursor, width);
        let (last_row, _) = visual_position(&self.text, self.text.len(), width);
        let total_rows = last_row.saturating_add(1).min(usize::from(u16::MAX)) as u16;
        let rows = total_rows.clamp(1, maximum_rows);
        let cursor_row = cursor_row.min(usize::from(u16::MAX)) as u16;
        let scroll = cursor_row.saturating_sub(rows.saturating_sub(1));
        ComposerViewport {
            rows,
            scroll,
            cursor_row: cursor_row.saturating_sub(scroll),
            cursor_column: cursor_column.min(usize::from(u16::MAX)) as u16,
        }
    }

    pub(super) fn visible_lines(&self, width: u16, viewport: ComposerViewport) -> Vec<String> {
        visual_window(
            &self.text,
            usize::from(width.max(1)),
            usize::from(viewport.scroll),
            usize::from(viewport.rows),
        )
    }

    pub(super) fn place_visual_cursor(
        &mut self,
        visible_row: usize,
        column: usize,
        width: u16,
        scroll: u16,
        select: bool,
    ) {
        let old = self.cursor;
        let target_row = visible_row.saturating_add(usize::from(scroll));
        self.cursor =
            byte_at_visual_position(&self.text, target_row, column, usize::from(width.max(1)));
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

fn visual_position(value: &str, byte: usize, width: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut column = 0usize;
    for (index, character) in value.char_indices() {
        if index == byte {
            break;
        }
        advance_visual(character, width, &mut row, &mut column);
    }
    (row, column)
}

fn visual_window(value: &str, width: usize, start: usize, count: usize) -> Vec<String> {
    let end = start.saturating_add(count);
    let mut lines = Vec::with_capacity(count);
    let mut row = 0usize;
    let mut column = 0usize;
    if start == 0 && count > 0 {
        lines.push(String::new());
    }
    for character in value.chars() {
        if character == '\n' {
            row = row.saturating_add(1);
            column = 0;
            push_visible_row(&mut lines, row, start, end);
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if column > 0 && column.saturating_add(character_width) > width {
            row = row.saturating_add(1);
            column = 0;
            push_visible_row(&mut lines, row, start, end);
        }
        if row >= start && row < end {
            lines[row - start].push(character);
        }
        column = column.saturating_add(character_width);
        if column >= width {
            row = row.saturating_add(1);
            column = 0;
            push_visible_row(&mut lines, row, start, end);
        }
        if row >= end {
            break;
        }
    }
    lines
}

fn push_visible_row(lines: &mut Vec<String>, row: usize, start: usize, end: usize) {
    if row >= start && row < end {
        lines.push(String::new());
    }
}

fn byte_at_visual_position(
    value: &str,
    target_row: usize,
    target_column: usize,
    width: usize,
) -> usize {
    let mut row = 0usize;
    let mut column = 0usize;
    let mut last_on_row = 0usize;
    for (index, character) in value.char_indices() {
        if row == target_row {
            last_on_row = index;
            if column >= target_column {
                return index;
            }
        } else if row > target_row {
            return last_on_row;
        }
        advance_visual(character, width, &mut row, &mut column);
        if row == target_row {
            last_on_row = index + character.len_utf8();
        }
    }
    if row <= target_row {
        value.len()
    } else {
        last_on_row
    }
}

fn advance_visual(character: char, width: usize, row: &mut usize, column: &mut usize) {
    if character == '\n' {
        *row = row.saturating_add(1);
        *column = 0;
        return;
    }
    let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
    if *column > 0 && column.saturating_add(character_width) > width {
        *row = row.saturating_add(1);
        *column = 0;
    }
    *column = column.saturating_add(character_width);
    if *column >= width {
        *row = row.saturating_add(1);
        *column = 0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_grows_to_six_rows_then_follows_the_cursor() {
        let mut composer = Composer::new();
        composer.replace(
            (0..8)
                .map(|index| format!("line-{index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );

        let viewport = composer.viewport(40, 6);

        assert_eq!(viewport.rows, 6);
        assert_eq!(viewport.scroll, 2);
        assert_eq!(viewport.cursor_row, 5);
        assert_eq!(viewport.cursor_column, 6);
    }

    #[test]
    fn visual_pointer_placement_accounts_for_wrapping_and_scroll() {
        let mut composer = Composer::new();
        composer.replace("abcdefghij\nsecond".to_owned());

        composer.place_visual_cursor(0, 2, 5, 1, false);

        assert_eq!(&composer.text[..composer.cursor], "abcdefg");
    }
}
