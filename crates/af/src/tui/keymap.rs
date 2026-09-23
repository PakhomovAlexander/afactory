//! Keys, the NORMAL-mode key sequence table, and the one-line prompt editor.
//!
//! There is one table: `gg`, `zo`, `]]` and `<C-w>l` mean the same thing in every pane, and a
//! key the table does not bind reaches the opened pane as a pane-local verb.

/// One decoded key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    /// A control chord named by its lower-case letter: `Ctrl('d')` is `<C-d>`.
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// Decode the bytes one terminal read returned. Escape sequences arrive whole in one read, so
/// an `ESC` that ends the chunk is the Escape key. Bytes outside printable ASCII that name no
/// key are dropped: nothing but printable ASCII ever reaches a prompt.
pub(crate) fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        let key = match byte {
            0x1b => match bytes.get(index).copied() {
                Some(intro) if intro == b'[' || intro == b'O' => {
                    index += 1;
                    let start = index;
                    while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                        index += 1;
                    }
                    let Some(&last) = bytes.get(index) else {
                        continue;
                    };
                    index += 1;
                    let parameters = &bytes[start..index - 1];
                    match (intro, last, parameters) {
                        (_, b'A', _) => Key::Up,
                        (_, b'B', _) => Key::Down,
                        (_, b'C', _) => Key::Right,
                        (_, b'D', _) => Key::Left,
                        (_, b'H', _) => Key::Home,
                        (_, b'F', _) => Key::End,
                        (b'[', b'Z', _) => Key::BackTab,
                        (b'[', b'~', b"1" | b"7") => Key::Home,
                        (b'[', b'~', b"4" | b"8") => Key::End,
                        (b'[', b'~', b"3") => Key::Delete,
                        (b'[', b'~', b"5") => Key::PageUp,
                        (b'[', b'~', b"6") => Key::PageDown,
                        _ => continue,
                    }
                }
                _ => Key::Esc,
            },
            b'\r' | b'\n' => Key::Enter,
            b'\t' => Key::Tab,
            0x7f | 0x08 => Key::Backspace,
            0x01..=0x1a => Key::Ctrl(char::from(b'a' + byte - 1)),
            0x20..=0x7e => Key::Char(char::from(byte)),
            _ => continue,
        };
        keys.push(key);
    }
    keys
}

/// What a NORMAL-mode key or key sequence asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Down,
    Up,
    Left,
    Right,
    Top,
    Bottom,
    HalfDown,
    HalfUp,
    PageDown,
    /// `<C-b>`: a page up in the main pane; on the bar it toggles the bar instead.
    PageUp,
    FoldOpen,
    FoldClose,
    FoldToggle,
    FoldOpenAll,
    FoldCloseAll,
    SearchForward,
    SearchBackward,
    SearchNext,
    SearchPrevious,
    FocusNext,
    FocusMain,
    FocusBar,
    NextFolder,
    PreviousFolder,
    Open,
    Yank,
    EditFile,
    Refresh,
    Quit,
    Command,
    Cancel,
    /// A key the table does not bind; the opened pane may use it as a pane-local verb.
    Pane(Key),
}

/// The NORMAL-mode parser: single keys map directly, and `g`, `z`, `]`, `[`, `Z` and `<C-w>`
/// wait for the key that completes them. An unknown continuation abandons the sequence, as vim
/// does, so the key after it starts afresh.
#[derive(Debug, Default)]
pub(crate) struct KeyMap {
    /// The first key of an unfinished sequence; `w` stands for `<C-w>`.
    pending: Option<char>,
}

impl KeyMap {
    pub(crate) fn feed(&mut self, key: Key) -> Option<Action> {
        if let Some(prefix) = self.pending.take() {
            return match (prefix, key) {
                ('g', Key::Char('g')) => Some(Action::Top),
                ('g', Key::Char('f')) => Some(Action::EditFile),
                ('z', Key::Char('o')) => Some(Action::FoldOpen),
                ('z', Key::Char('c')) => Some(Action::FoldClose),
                ('z', Key::Char('a')) => Some(Action::FoldToggle),
                ('z', Key::Char('R')) => Some(Action::FoldOpenAll),
                ('z', Key::Char('M')) => Some(Action::FoldCloseAll),
                (']', Key::Char(']')) => Some(Action::NextFolder),
                ('[', Key::Char('[')) => Some(Action::PreviousFolder),
                ('Z', Key::Char('Z')) => Some(Action::Quit),
                ('w', Key::Char('l') | Key::Ctrl('l') | Key::Right) => Some(Action::FocusMain),
                // `<C-h>` arrives as the backspace byte on most terminals.
                ('w', Key::Char('h') | Key::Backspace | Key::Left) => Some(Action::FocusBar),
                ('w', Key::Char('w') | Key::Ctrl('w')) => Some(Action::FocusNext),
                _ => None,
            };
        }
        let action = match key {
            Key::Char(prefix @ ('g' | 'z' | ']' | '[' | 'Z')) => {
                self.pending = Some(prefix);
                return None;
            }
            Key::Ctrl('w') => {
                self.pending = Some('w');
                return None;
            }
            Key::Char('j') | Key::Down => Action::Down,
            Key::Char('k') | Key::Up => Action::Up,
            Key::Char('h') | Key::Left => Action::Left,
            Key::Char('l') | Key::Right => Action::Right,
            Key::Char('G') | Key::End => Action::Bottom,
            Key::Home => Action::Top,
            Key::Ctrl('d') => Action::HalfDown,
            Key::Ctrl('u') => Action::HalfUp,
            Key::Ctrl('f') | Key::PageDown => Action::PageDown,
            Key::Ctrl('b') | Key::PageUp => Action::PageUp,
            Key::Char('/') => Action::SearchForward,
            Key::Char('?') => Action::SearchBackward,
            Key::Char('n') => Action::SearchNext,
            Key::Char('N') => Action::SearchPrevious,
            Key::Tab | Key::BackTab => Action::FocusNext,
            Key::Enter | Key::Char('o') => Action::Open,
            Key::Char('y') => Action::Yank,
            Key::Char('R') => Action::Refresh,
            Key::Char('q') => Action::Quit,
            Key::Char(':') => Action::Command,
            Key::Esc | Key::Ctrl('c') => Action::Cancel,
            other => Action::Pane(other),
        };
        Some(action)
    }

    /// The unfinished sequence, as the status line shows it.
    pub(crate) fn pending(&self) -> Option<&'static str> {
        match self.pending? {
            'g' => Some("g"),
            'z' => Some("z"),
            ']' => Some("]"),
            '[' => Some("["),
            'Z' => Some("Z"),
            'w' => Some("<C-w>"),
            _ => None,
        }
    }
}

/// What one key did to a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptEvent {
    Edited,
    Complete,
    Submit(String),
    Cancel,
}

/// The `:` and `/` line: printable ASCII, `<C-a>` `<C-e>` `<C-w>` `<C-u>`, `Esc` cancels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub(crate) text: String,
    /// Byte offset of the cursor; the text is ASCII, so bytes are columns.
    pub(crate) cursor: usize,
}

impl Prompt {
    pub(crate) fn key(&mut self, key: Key) -> PromptEvent {
        match key {
            Key::Enter => return PromptEvent::Submit(std::mem::take(&mut self.text)),
            Key::Esc | Key::Ctrl('c') => return PromptEvent::Cancel,
            Key::Tab => return PromptEvent::Complete,
            Key::Backspace if self.text.is_empty() => return PromptEvent::Cancel,
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
            }
            Key::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = self.text.len(),
            Key::Ctrl('u') => {
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            Key::Ctrl('w') => {
                let kept = self.text[..self.cursor].trim_end_matches(' ');
                let start = kept.rfind(' ').map_or(0, |space| space + 1);
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            Key::Char(character) if character.is_ascii() && !character.is_ascii_control() => {
                self.text.insert(self.cursor, character);
                self.cursor += 1;
            }
            _ => {}
        }
        PromptEvent::Edited
    }

    pub(crate) fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(keys: &[Key]) -> Vec<Option<Action>> {
        let mut map = KeyMap::default();
        keys.iter().map(|key| map.feed(*key)).collect()
    }

    fn chars(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    #[test]
    fn single_keys_map_directly() {
        let bound = [
            (Key::Char('j'), Action::Down),
            (Key::Char('k'), Action::Up),
            (Key::Char('h'), Action::Left),
            (Key::Char('l'), Action::Right),
            (Key::Char('G'), Action::Bottom),
            (Key::Ctrl('d'), Action::HalfDown),
            (Key::Ctrl('u'), Action::HalfUp),
            (Key::Ctrl('f'), Action::PageDown),
            (Key::Ctrl('b'), Action::PageUp),
            (Key::Char('/'), Action::SearchForward),
            (Key::Char('?'), Action::SearchBackward),
            (Key::Char('n'), Action::SearchNext),
            (Key::Char('N'), Action::SearchPrevious),
            (Key::Tab, Action::FocusNext),
            (Key::Enter, Action::Open),
            (Key::Char('o'), Action::Open),
            (Key::Char('y'), Action::Yank),
            (Key::Char('R'), Action::Refresh),
            (Key::Char('q'), Action::Quit),
            (Key::Char(':'), Action::Command),
            (Key::Esc, Action::Cancel),
            (Key::Ctrl('c'), Action::Cancel),
            (Key::Char('e'), Action::Pane(Key::Char('e'))),
        ];
        for (key, action) in bound {
            assert_eq!(actions(&[key]), vec![Some(action)], "{key:?}");
        }
    }

    #[test]
    fn two_key_sequences_complete() {
        let sequences = [
            ("gg", Action::Top),
            ("gf", Action::EditFile),
            ("zo", Action::FoldOpen),
            ("zc", Action::FoldClose),
            ("za", Action::FoldToggle),
            ("zR", Action::FoldOpenAll),
            ("zM", Action::FoldCloseAll),
            ("]]", Action::NextFolder),
            ("[[", Action::PreviousFolder),
            ("ZZ", Action::Quit),
        ];
        for (keys, action) in sequences {
            assert_eq!(actions(&chars(keys)), vec![None, Some(action)], "{keys}");
        }
        let windows = [
            (Key::Char('l'), Action::FocusMain),
            (Key::Right, Action::FocusMain),
            (Key::Char('h'), Action::FocusBar),
            (Key::Backspace, Action::FocusBar),
            (Key::Char('w'), Action::FocusNext),
        ];
        for (key, action) in windows {
            let fed = actions(&[Key::Ctrl('w'), key]);
            assert_eq!(fed, vec![None, Some(action)], "<C-w>{key:?}");
        }
    }

    #[test]
    fn an_unknown_continuation_abandons_the_sequence() {
        // `gj` is nothing; the `j` after it moves again.
        let fed = actions(&chars("gjj"));
        assert_eq!(fed, vec![None, None, Some(Action::Down)]);
        assert_eq!(actions(&chars("zx")), vec![None, None]);
        let fed = actions(&chars("]ggg"));
        assert_eq!(fed, vec![None, None, None, Some(Action::Top)]);
        let fed = actions(&[Key::Char('Z'), Key::Esc, Key::Char('q')]);
        assert_eq!(fed, vec![None, None, Some(Action::Quit)]);
        let mut map = KeyMap::default();
        assert_eq!(map.feed(Key::Ctrl('w')), None);
        assert_eq!(map.pending(), Some("<C-w>"));
        assert_eq!(map.feed(Key::Char('l')), Some(Action::FocusMain));
        assert_eq!(map.pending(), None);
    }

    #[test]
    fn terminal_bytes_decode_to_keys() {
        let bytes = b"gg\x1b[A\x1b[B\x1bOC\x1b[D\x1b[5~\x1b[6~\x1b[Z\r\t\x7f\x04\x17";
        let expected = vec![
            Key::Char('g'),
            Key::Char('g'),
            Key::Up,
            Key::Down,
            Key::Right,
            Key::Left,
            Key::PageUp,
            Key::PageDown,
            Key::BackTab,
            Key::Enter,
            Key::Tab,
            Key::Backspace,
            Key::Ctrl('d'),
            Key::Ctrl('w'),
        ];
        assert_eq!(decode(bytes), expected);
        // A lone ESC is the Escape key; non-ASCII bytes and unknown sequences name nothing.
        assert_eq!(decode(b"\x1b"), vec![Key::Esc]);
        assert_eq!(decode("\u{e9}\x1b[99x".as_bytes()), Vec::<Key>::new());
    }

    #[test]
    fn the_prompt_edits_like_a_shell_line() {
        let mut prompt = Prompt::default();
        for key in chars("scope user") {
            assert_eq!(prompt.key(key), PromptEvent::Edited);
        }
        assert_eq!(prompt.key(Key::Ctrl('w')), PromptEvent::Edited);
        assert_eq!(prompt.text, "scope ");
        prompt.key(Key::Ctrl('a'));
        prompt.key(Key::Char('x'));
        assert_eq!(prompt.text, "xscope ");
        prompt.key(Key::Ctrl('e'));
        prompt.key(Key::Char('p'));
        prompt.key(Key::Left);
        prompt.key(Key::Ctrl('u'));
        assert_eq!((prompt.text.as_str(), prompt.cursor), ("p", 0));
        prompt.key(Key::Char('\u{e9}'));
        assert_eq!(prompt.text, "p", "non-ASCII never enters a prompt");
        assert_eq!(prompt.key(Key::Tab), PromptEvent::Complete);
        assert_eq!(prompt.key(Key::Enter), PromptEvent::Submit("p".into()));
        assert_eq!(prompt.key(Key::Backspace), PromptEvent::Cancel);
        assert_eq!(prompt.key(Key::Esc), PromptEvent::Cancel);
    }
}
