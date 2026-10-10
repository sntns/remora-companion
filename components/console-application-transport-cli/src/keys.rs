use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What a key means here: bytes for the device, or one of the escape
/// commands picocom's users have in their fingers.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Send(Vec<u8>),
    Quit,
    Break,
    /// The escape key was pressed: what follows is a command.
    Escape,
    /// Nothing to do (a release, an unbound key).
    None,
}

/// The escape key: C-a, as picocom and screen.
const ESCAPE: u8 = 0x01;

/// Reads keys, `escaped` once the escape key came first.
#[derive(Default)]
pub(crate) struct Keyboard {
    escaped: bool,
}

impl Keyboard {
    pub(crate) fn escaped(&self) -> bool {
        self.escaped
    }

    /// `application_cursor`: the device asked for the arrows' application
    /// form (DECCKM), as full-screen programs do.
    pub(crate) fn action(&mut self, key: KeyEvent, application_cursor: bool) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let Some(bytes) = encode(key, application_cursor) else {
            return Action::None;
        };
        if std::mem::take(&mut self.escaped) {
            // C-a then: x or C-x quits, b or C-b breaks, C-a sends one.
            return match bytes.as_slice() {
                [b'x' | b'X' | 0x18] => Action::Quit,
                [b'b' | b'B' | 0x02] => Action::Break,
                [ESCAPE | b'a'] => Action::Send(vec![ESCAPE]),
                _ => Action::None,
            };
        }
        if bytes == [ESCAPE] {
            self.escaped = true;
            return Action::Escape;
        }
        Action::Send(bytes)
    }
}

/// The bytes a VT100-family terminal sends for `key`.
pub(crate) fn encode(key: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let cursor = |letter: u8| {
        if application_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let tilde = |number: &str| format!("\x1b[{number}~").into_bytes();
    let mut bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => control(c)?,
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor(b'A'),
        KeyCode::Down => cursor(b'B'),
        KeyCode::Right => cursor(b'C'),
        KeyCode::Left => cursor(b'D'),
        KeyCode::Home => cursor(b'H'),
        KeyCode::End => cursor(b'F'),
        KeyCode::Insert => tilde("2"),
        KeyCode::Delete => tilde("3"),
        KeyCode::PageUp => tilde("5"),
        KeyCode::PageDown => tilde("6"),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + n - 1],
        KeyCode::F(n @ 5..=12) => {
            tilde(["15", "17", "18", "19", "20", "21", "23", "24"][usize::from(n - 5)])
        }
        _ => return None,
    };
    // Alt as a terminal sends it: ESC first.
    if key.modifiers.contains(KeyModifiers::ALT) && !bytes.starts_with(&[0x1b]) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// C-@ to C-_, as a terminal makes them.
fn control(c: char) -> Option<Vec<u8>> {
    let byte = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        _ => return None,
    };
    Some(vec![byte])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_as_a_vt100_sends_them() {
        let plain = KeyModifiers::NONE;
        assert_eq!(
            encode(key(KeyCode::Char('é'), plain), false).unwrap(),
            "é".as_bytes()
        );
        assert_eq!(encode(key(KeyCode::Enter, plain), false).unwrap(), b"\r");
        assert_eq!(
            encode(key(KeyCode::Backspace, plain), false).unwrap(),
            [0x7f]
        );
        assert_eq!(
            encode(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(),
            [3]
        );
        assert_eq!(encode(key(KeyCode::Up, plain), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(key(KeyCode::Up, plain), true).unwrap(), b"\x1bOA");
        assert_eq!(encode(key(KeyCode::F(1), plain), false).unwrap(), b"\x1bOP");
        assert_eq!(
            encode(key(KeyCode::F(5), plain), false).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            encode(key(KeyCode::Char('b'), KeyModifiers::ALT), false).unwrap(),
            b"\x1bb"
        );
    }

    #[test]
    fn c_a_escapes_like_picocom() {
        let mut keyboard = Keyboard::default();
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(keyboard.action(ctrl('a'), false), Action::Escape);
        assert!(keyboard.escaped());
        assert_eq!(keyboard.action(ctrl('x'), false), Action::Quit);
        assert_eq!(keyboard.action(ctrl('a'), false), Action::Escape);
        assert_eq!(
            keyboard.action(key(KeyCode::Char('b'), KeyModifiers::NONE), false),
            Action::Break
        );
        assert_eq!(keyboard.action(ctrl('a'), false), Action::Escape);
        assert_eq!(keyboard.action(ctrl('a'), false), Action::Send(vec![1]));
        // Anything else after C-a is dropped, then keys flow again.
        assert_eq!(keyboard.action(ctrl('a'), false), Action::Escape);
        assert_eq!(
            keyboard.action(key(KeyCode::Char('q'), KeyModifiers::NONE), false),
            Action::None
        );
        assert_eq!(
            keyboard.action(key(KeyCode::Char('q'), KeyModifiers::NONE), false),
            Action::Send(b"q".to_vec())
        );
    }
}
