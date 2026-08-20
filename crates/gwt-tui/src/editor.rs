//! A small multi-line text buffer with a cursor.
//!
//! Every other prompt in these screens is append-and-backspace, which is fine
//! for a path or a branch name. A `run` step's command is a shell script, and a
//! script you cannot move around in is a script you retype — so this one field
//! gets arrow keys, Home/End, and Delete.
//!
//! The cursor is a byte offset that always sits on a char boundary; every method
//! that moves it does so by whole characters, so multi-byte text is safe.

/// A text buffer with a cursor. `\n` is an ordinary character in it: what makes
/// this multi-line is that the caller renders `lines()` rather than one string.
#[derive(Debug, Clone, Default)]
pub struct TextArea {
    text: String,
    cursor: usize,
}

impl TextArea {
    /// A buffer holding `text`, with the cursor at its end — where someone who
    /// pressed `e` to fix a typo expects to start.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether there is anything but whitespace in the buffer.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        let Some(prev) = self.prev_boundary(self.cursor) else {
            return;
        };
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        let Some(next) = self.next_boundary(self.cursor) else {
            return;
        };
        self.text.replace_range(self.cursor..next, "");
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    /// Move to the same column one line up, clamped to that line's length.
    pub fn up(&mut self) {
        let start = self.line_start(self.cursor);
        if start == 0 {
            self.cursor = 0;
            return;
        }
        let col = self.text[start..self.cursor].chars().count();
        let prev_start = self.line_start(start - 1);
        self.cursor = self.column_offset(prev_start, start - 1, col);
    }

    pub fn down(&mut self) {
        let start = self.line_start(self.cursor);
        let end = self.line_end(self.cursor);
        if end >= self.text.len() {
            self.cursor = self.text.len();
            return;
        }
        let col = self.text[start..self.cursor].chars().count();
        let next_start = end + 1;
        let next_end = self.line_end(next_start);
        self.cursor = self.column_offset(next_start, next_end, col);
    }

    pub fn lines(&self) -> std::str::Split<'_, char> {
        self.text.split('\n')
    }

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// Where the caret is, as `(line index, column in characters)` — what a
    /// renderer needs to split the line it is drawing.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let start = self.line_start(self.cursor);
        let line = self.text[..start].matches('\n').count();
        (line, self.text[start..self.cursor].chars().count())
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        let c = self.text[..at].chars().next_back()?;
        Some(at - c.len_utf8())
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        let c = self.text[at..].chars().next()?;
        Some(at + c.len_utf8())
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map(|i| at + i)
            .unwrap_or(self.text.len())
    }

    /// The offset `col` characters into `start..end`, or `end` if the line is
    /// shorter than that.
    fn column_offset(&self, start: usize, end: usize, col: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(col)
            .map(|(i, _)| start + i)
            .unwrap_or(end)
    }
}

#[cfg(test)]
mod tests {
    use super::TextArea;

    #[test]
    fn typing_and_backspace_walk_whole_characters() {
        let mut t = TextArea::default();
        for c in "npm ci".chars() {
            t.insert(c);
        }
        t.backspace();
        assert_eq!(t.text(), "npm c");
        // Multi-byte text must not be cut in half.
        for c in "あい".chars() {
            t.insert(c);
        }
        t.backspace();
        assert_eq!(t.text(), "npm cあ");
    }

    #[test]
    fn newlines_make_lines() {
        let mut t = TextArea::new("set -e");
        t.insert('\n');
        for c in "npm ci".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "set -e\nnpm ci");
        assert_eq!(t.lines().collect::<Vec<_>>(), vec!["set -e", "npm ci"]);
        assert_eq!(t.cursor_line_col(), (1, 6));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_one_above() {
        let mut t = TextArea::new("a\nb");
        t.home();
        assert_eq!(t.cursor_line_col(), (1, 0));
        t.backspace();
        assert_eq!(t.text(), "ab");
        assert_eq!(t.cursor_line_col(), (0, 1));
    }

    #[test]
    fn vertical_movement_keeps_the_column_where_it_can() {
        let mut t = TextArea::new("longer line\nab\nlast");
        // Cursor is at the end of "last" (col 4).
        t.up();
        // "ab" is shorter, so the cursor lands at its end.
        assert_eq!(t.cursor_line_col(), (1, 2));
        t.up();
        assert_eq!(t.cursor_line_col(), (0, 2));
        t.down();
        t.down();
        assert_eq!(t.cursor_line_col(), (2, 2));
        t.end();
        assert_eq!(t.cursor_line_col(), (2, 4));
    }

    #[test]
    fn editing_happens_at_the_cursor_not_the_end() {
        let mut t = TextArea::new("npm ci");
        t.home();
        t.insert('!');
        assert_eq!(t.text(), "!npm ci");
        t.delete();
        assert_eq!(t.text(), "!pm ci");
        t.right();
        t.insert('-');
        assert_eq!(t.text(), "!p-m ci");
    }

    #[test]
    fn moving_past_either_end_stays_put() {
        let mut t = TextArea::new("x");
        t.home();
        t.left();
        t.backspace();
        assert_eq!(t.text(), "x");
        t.end();
        t.right();
        t.delete();
        assert_eq!(t.text(), "x");
    }
}
