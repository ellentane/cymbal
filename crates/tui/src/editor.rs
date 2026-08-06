#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    lines: Vec<String>,
    x: usize,
    y: usize,
}

impl Editor {
    pub fn new(content: String) -> Self {
        let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        Self { lines, x: 0, y: 0 }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.x, self.y)
    }

    pub fn content(&self) -> String {
        self.lines.join("\n")
    }

    fn clamp_x(&mut self) {
        let len = self.lines[self.y].chars().count();
        if self.x > len {
            self.x = len;
        }
    }

    pub fn set_content(&mut self, content: String) {
        self.lines = content.split('\n').map(|s| s.to_string()).collect();
        self.x = 0;
        self.y = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        let idx = self.lines[self.y]
            .char_indices()
            .nth(self.x)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[self.y].len());
        self.lines[self.y].insert(idx, c);
        self.x += 1;
    }

    pub fn newline(&mut self) {
        let idx = self.lines[self.y]
            .char_indices()
            .nth(self.x)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[self.y].len());
        let rest = self.lines[self.y].split_off(idx);
        self.lines.insert(self.y + 1, rest);
        self.x = 0;
        self.y += 1;
    }

    pub fn backspace(&mut self) {
        if self.x > 0 {
            let idx = self.lines[self.y]
                .char_indices()
                .nth(self.x - 1)
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.lines[self.y].remove(idx);
            self.x -= 1;
        } else if self.y > 0 {
            let prev_len = self.lines[self.y - 1].chars().count();
            let rest = self.lines.remove(self.y);
            self.lines[self.y - 1].push_str(&rest);
            self.y -= 1;
            self.x = prev_len;
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    pub fn delete_word_before_cursor(&mut self) {
        if self.x == 0 {
            return;
        }
        let line = self.lines[self.y].clone();
        let byte_idx = line
            .char_indices()
            .nth(self.x)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let before = &line[..byte_idx];
        let start_byte = before
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '#')
            .map_or(0, |i| i + 1);
        self.lines[self.y].replace_range(start_byte..byte_idx, "");
        self.x = before[..start_byte].chars().count();
    }

    pub fn move_left(&mut self) {
        if self.x > 0 {
            self.x -= 1;
        }
    }

    pub fn move_right(&mut self) {
        self.x += 1;
        self.clamp_x();
    }

    pub fn move_up(&mut self) {
        if self.y > 0 {
            self.y -= 1;
        }
        self.clamp_x();
    }

    pub fn move_down(&mut self) {
        if self.y + 1 < self.lines.len() {
            self.y += 1;
        }
        self.clamp_x();
    }

    pub fn move_home(&mut self) {
        self.x = 0;
    }

    pub fn move_end(&mut self) {
        self.x = self.lines[self.y].chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_newline() {
        let mut e = Editor::new("ab\ncd".to_string());
        e.insert_char('x'); // cursor at 0,0
        assert_eq!(e.lines(), vec!["xab".to_string(), "cd".to_string()]);
        e.move_down();
        e.move_end();
        e.newline();
        assert_eq!(
            e.lines(),
            vec!["xab".to_string(), "cd".to_string(), "".to_string()]
        );
    }

    #[test]
    fn backspace_joins_lines() {
        let mut e = Editor::new("ab\ncd".to_string());
        e.move_down();
        e.move_home();
        e.backspace();
        assert_eq!(e.lines(), vec!["abcd".to_string()]);
        assert_eq!(e.y, 0);
        assert_eq!(e.x, 2);
    }

    #[test]
    fn cursor_clamped_to_lines() {
        let mut e = Editor::new("ab\nc".to_string());
        e.move_up(); // y=0
        e.move_right();
        e.move_right();
        e.move_right();
        e.move_right();
        assert_eq!(e.x, 2);
        e.move_down();
        e.move_down();
        e.move_down();
        assert_eq!(e.y, 1);
    }

    #[test]
    fn set_content_replaces_text_and_clamps_cursor() {
        let mut e = Editor::new("ab\ncd".to_string());
        e.move_down();
        e.move_right();
        e.set_content("x\nyz".to_string());
        assert_eq!(e.content(), "x\nyz");
        assert_eq!(e.y, 0, "cursor resets to the top");
        assert_eq!(e.x, 0);
    }

    #[test]
    fn content_roundtrips() {
        let src = "tempo 120\nloop \"b\":\n    kick << \"x . . x\"\n";
        let mut e = Editor::new(src.to_string());
        assert_eq!(e.content(), src);
        e.insert_char(' '); // at 0,0: " tempo 120"
        assert_eq!(
            e.content(),
            " tempo 120\nloop \"b\":\n    kick << \"x . . x\"\n"
        );
    }

    #[test]
    fn insert_str_after_delete_word_replaces_prefix() {
        let mut e = Editor::new("    kick << \"x\" ve\n".to_string());
        e.move_end();
        e.insert_str("l");
        assert_eq!(e.content(), "    kick << \"x\" vel\n");
    }

    #[test]
    fn delete_word_before_cursor_removes_word() {
        let mut e = Editor::new("    kick << \"x\" vel\n".to_string());
        e.move_end();
        e.delete_word_before_cursor();
        assert_eq!(e.content(), "    kick << \"x\" \n");
        assert_eq!(e.x, 16);
    }

    #[test]
    fn delete_word_before_cursor_does_nothing_at_line_start() {
        let mut e = Editor::new("kick\n".to_string());
        e.delete_word_before_cursor();
        assert_eq!(e.content(), "kick\n");
        assert_eq!(e.x, 0);
    }
}
