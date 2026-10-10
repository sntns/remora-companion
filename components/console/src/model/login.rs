/// The challenge a Remora device's console login shows, as remora-accessd's
/// `remora-login challenge` prints it:
///
/// ```text
/// Remora local login: root@E2ETEST0002
/// Challenge: K7QM-3XRB
/// Type the code for this challenge (or an offline code) at the password prompt.
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginPrompt {
    /// The device's name, as the platform knows it.
    pub device: String,
    /// The account being logged into.
    pub account: String,
    /// As shown: `XXXX-XXXX`.
    pub challenge: String,
}

const LOGIN_MARKER: &str = "Remora local login: ";
const CHALLENGE_MARKER: &str = "Challenge: ";

impl LoginPrompt {
    /// The last challenge `screen` shows, one line per row as the screen
    /// holds them: a newer one below an older, refused one is the one that
    /// counts.
    pub fn find(screen: &str) -> Option<Self> {
        let lines: Vec<&str> = screen.lines().collect();
        lines.iter().enumerate().rev().find_map(|(index, line)| {
            let challenge = line.trim().strip_prefix(CHALLENGE_MARKER)?.trim();
            if !is_challenge(challenge) {
                return None;
            }
            let heading = lines[..index].last()?.trim();
            let (account, device) = heading.strip_prefix(LOGIN_MARKER)?.split_once('@')?;
            let (account, device) = (account.trim(), device.trim());
            if account.is_empty() || device.is_empty() || device.contains(char::is_whitespace) {
                return None;
            }
            Some(Self {
                device: device.to_owned(),
                account: account.to_owned(),
                challenge: challenge.to_owned(),
            })
        })
    }
}

/// `XXXX-XXXX`, letters and digits.
fn is_challenge(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || b.is_ascii_alphanumeric())
}

/// Whether the line the cursor is on is a password prompt, waiting.
pub fn awaits_password(cursor_line: &str) -> bool {
    cursor_line
        .trim_end()
        .to_ascii_lowercase()
        .ends_with("password:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOWN: &str = "E2ETEST0002 login: root\n\
        Remora local login: root@E2ETEST0002\n\
        Challenge: K7QM-3XRB\n\
        Type the code for this challenge (or an offline code) at the password prompt.\n\
        Password: ";

    #[test]
    fn reads_the_challenge_off_the_screen() {
        assert_eq!(
            LoginPrompt::find(SHOWN),
            Some(LoginPrompt {
                device: "E2ETEST0002".into(),
                account: "root".into(),
                challenge: "K7QM-3XRB".into(),
            })
        );
    }

    #[test]
    fn the_newest_challenge_counts() {
        let again = format!(
            "{SHOWN}\n\nLogin incorrect\nE2ETEST0002 login: admin\n\
             Remora local login: admin@E2ETEST0002\nChallenge: AB12-CD34\nPassword: "
        );
        let prompt = LoginPrompt::find(&again).unwrap();
        assert_eq!(prompt.account, "admin");
        assert_eq!(prompt.challenge, "AB12-CD34");
    }

    #[test]
    fn nothing_that_only_looks_like_one() {
        for screen in [
            "Challenge: K7QM-3XRB",
            "Remora local login: root@E2ETEST0002\nChallenge: K7QM3XRB",
            "Remora local login: unavailable, this device has no login secret yet.\nChallenge: K7QM-3XRB",
            "echo Challenge: K7QM-3XRB",
        ] {
            assert_eq!(LoginPrompt::find(screen), None, "{screen:?}");
        }
    }

    #[test]
    fn a_password_prompt_waits_at_the_cursor() {
        assert!(awaits_password("Password: "));
        assert!(awaits_password("root@E2ETEST0002's password:"));
        assert!(!awaits_password("Password: ********"));
        assert!(!awaits_password("E2ETEST0002 login: "));
    }
}
