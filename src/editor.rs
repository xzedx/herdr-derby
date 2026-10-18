//! A small multi-line text buffer for the prompt, with soft wrapping by
//! display width so CJK text lines up.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Editor {
    buf: Vec<char>,
    cur: usize,
}

fn width(c: char) -> usize {
    c.width().unwrap_or(0)
}

impl Editor {
    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn is_blank(&self) -> bool {
        self.buf.iter().all(|c| c.is_whitespace())
    }

    pub fn set(&mut self, text: &str) {
        self.buf = text.chars().collect();
        self.cur = self.buf.len();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
    }

    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cur, c);
        self.cur += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            // Tabs would throw off the width math; control chars are noise.
            match c {
                '\t' => {
                    for _ in 0..4 {
                        self.insert(' ');
                    }
                }
                '\n' => self.insert('\n'),
                c if c.is_control() => {}
                c => self.insert(c),
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
            self.buf.remove(self.cur);
        }
    }

    pub fn delete(&mut self) {
        if self.cur < self.buf.len() {
            self.buf.remove(self.cur);
        }
    }

    pub fn left(&mut self) {
        self.cur = self.cur.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cur = (self.cur + 1).min(self.buf.len());
    }

    fn line_start(&self, at: usize) -> usize {
        self.buf[..at]
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.buf[at..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(self.buf.len(), |i| at + i)
    }

    pub fn home(&mut self) {
        self.cur = self.line_start(self.cur);
    }

    pub fn end(&mut self) {
        self.cur = self.line_end(self.cur);
    }

    /// Move to the previous logical line, keeping the column in chars.
    /// Returns false on the first line, so the caller can use Up for
    /// something else (recalling the last prompt).
    pub fn up(&mut self) -> bool {
        let start = self.line_start(self.cur);
        if start == 0 {
            return false;
        }
        let col = self.cur - start;
        let prev_start = self.line_start(start - 1);
        self.cur = (prev_start + col).min(start - 1);
        true
    }

    pub fn down(&mut self) -> bool {
        let end = self.line_end(self.cur);
        if end == self.buf.len() {
            return false;
        }
        let col = self.cur - self.line_start(self.cur);
        let next_start = end + 1;
        self.cur = (next_start + col).min(self.line_end(next_start));
        true
    }

    /// Soft-wrap into rows of at most `cols` display cells. Returns the rows
    /// and the cursor's (row, col) in cells.
    pub fn layout(&self, cols: usize) -> (Vec<String>, (usize, usize)) {
        let cols = cols.max(2);
        let mut rows = vec![String::new()];
        let mut col = 0;
        let mut cursor = (0, 0);
        for (i, &c) in self.buf.iter().enumerate() {
            if c != '\n' && col + width(c) > cols {
                rows.push(String::new());
                col = 0;
            }
            if i == self.cur {
                cursor = (rows.len() - 1, col);
            }
            if c == '\n' {
                rows.push(String::new());
                col = 0;
            } else {
                rows.last_mut().unwrap().push(c);
                col += width(c);
            }
        }
        if self.cur == self.buf.len() {
            if col >= cols {
                rows.push(String::new());
                col = 0;
            }
            cursor = (rows.len() - 1, col);
        }
        (rows, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_by_display_width() {
        let mut e = Editor::default();
        e.insert_str("赛马赛马ab");
        // Each CJK char is two cells: 4 cells per row at cols=4.
        let (rows, cursor) = e.layout(4);
        assert_eq!(rows, vec!["赛马", "赛马", "ab"]);
        assert_eq!(cursor, (2, 2));
    }

    #[test]
    fn newline_and_cursor() {
        let mut e = Editor::default();
        e.insert_str("ab\ncd");
        e.left();
        let (rows, cursor) = e.layout(10);
        assert_eq!(rows, vec!["ab", "cd"]);
        assert_eq!(cursor, (1, 1));
    }

    #[test]
    fn cursor_wraps_when_row_is_full() {
        let mut e = Editor::default();
        e.insert_str("abcd");
        let (rows, cursor) = e.layout(4);
        assert_eq!(rows, vec!["abcd", ""]);
        assert_eq!(cursor, (1, 0));
    }

    #[test]
    fn up_down_keep_column() {
        let mut e = Editor::default();
        e.insert_str("hello\nhi\nworld");
        assert!(e.up());
        assert_eq!(e.layout(20).1, (1, 2)); // clamped to end of "hi"
        assert!(e.up());
        assert_eq!(e.layout(20).1, (0, 2));
        assert!(!e.up());
        assert!(e.down());
        assert!(e.down());
        assert!(!e.down());
    }

    #[test]
    fn paste_drops_controls_and_expands_tabs() {
        let mut e = Editor::default();
        e.insert_str("a\tb\x07c");
        assert_eq!(e.text(), "a    bc");
    }
}
