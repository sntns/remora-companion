use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use remora_channel::model::SshRole;
use remora_console::{
    application::{ConsoleService, ConsoleSession},
    model::{ConsoleEvent, ConsoleRequest, SerialSettings},
};
use remora_context::model::ContextOverride;

use crate::{
    error::{console_error, Error, Result},
    keys::{Action, Keyboard},
    terminal::Terminal,
};

#[derive(clap::Args)]
#[command(
    after_help = "Keys: C-a x quits, C-a b sends a break, C-a C-a sends C-a.\n\n\
Example:\n  rmra local console /dev/ttyUSB0 --role admin"
)]
pub struct ConsoleArgs {
    /// The serial port the device's console is on: /dev/ttyUSB0, COM3...
    #[arg(value_hint = clap::ValueHint::FilePath)]
    port: String,
    /// Its speed; always 8N1, no flow control.
    #[arg(long, default_value_t = 115_200)]
    baud: u32,
    /// The role a login challenge is answered as: when the console shows
    /// one, its code is asked of the platform and typed at the password
    /// prompt. It opens any account the device maps to this role.
    #[arg(long, value_enum, default_value = "user")]
    role: Role,
    /// Leave login challenges to you: no code is asked for or typed.
    #[arg(long)]
    no_login: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Role {
    User,
    Admin,
}

impl From<Role> for SshRole {
    fn from(role: Role) -> Self {
        match role {
            Role::User => SshRole::User,
            Role::Admin => SshRole::Admin,
        }
    }
}

/// `rmra local console`: the device's serial console on this terminal,
/// drawn from its screen model, until C-a x or the port goes away.
pub async fn run(
    args: ConsoleArgs,
    service: &ConsoleService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let (rows, cols) = Terminal::size()?;
    let login = (!args.no_login).then(|| SshRole::from(args.role));
    let settings = SerialSettings {
        port: args.port,
        baud: args.baud,
    };
    let mut session = service
        .open(ConsoleRequest {
            over: over.cloned(),
            settings: settings.clone(),
            rows: rows.saturating_sub(1).max(1),
            cols,
            login,
        })
        .await
        .map_err(console_error)?;

    let mut terminal = Terminal::enter()?;
    let mut status = Status::new(&settings, login);
    let ended = drive(&mut *session, &mut terminal, &mut status).await;
    drop(terminal);
    match ended? {
        Ending::Quit => Ok(()),
        Ending::Closed => Err(error_stack::Report::new(Error::Console(format!(
            "{} went away",
            settings.port
        )))),
    }
}

enum Ending {
    Quit,
    Closed,
}

async fn drive(
    session: &mut dyn ConsoleSession,
    terminal: &mut Terminal,
    status: &mut Status,
) -> Result<Ending> {
    let mut keyboard = Keyboard::default();
    let mut events = EventStream::new();
    terminal.draw(session.screen(), &status.line(false))?;
    loop {
        tokio::select! {
            event = session.next() => {
                match event.map_err(console_error)? {
                    ConsoleEvent::Closed => return Ok(Ending::Closed),
                    ConsoleEvent::Screen => {}
                    event => status.note(event),
                }
            }
            input = events.next() => {
                let Some(input) = input else { return Ok(Ending::Quit) };
                let input = input.map_err(|error| error_stack::Report::new(error).change_context(Error::Terminal))?;
                match input {
                    Event::Key(key) => {
                        match keyboard.action(key, session.screen().application_cursor()) {
                            Action::Send(bytes) => session.send(&bytes).await.map_err(console_error)?,
                            Action::Quit => return Ok(Ending::Quit),
                            Action::Break => {
                                session.send_break().await.map_err(console_error)?;
                                status.message = "break sent".into();
                            }
                            Action::Escape | Action::None => {}
                        }
                    }
                    Event::Paste(text) => {
                        // A paste is typed, its newlines as Enter.
                        let text = text.replace("\r\n", "\r").replace('\n', "\r");
                        session.send(text.as_bytes()).await.map_err(console_error)?;
                    }
                    Event::Resize(cols, rows) => {
                        session.resize(rows.saturating_sub(1).max(1), cols);
                        terminal.invalidate();
                    }
                    _ => {}
                }
            }
        }
        terminal.draw(session.screen(), &status.line(keyboard.escaped()))?;
    }
}

/// The line under the console: where it is, how login is handled, the
/// last thing worth saying, and the keys.
struct Status {
    port: String,
    login: String,
    message: String,
}

impl Status {
    fn new(settings: &SerialSettings, login: Option<SshRole>) -> Self {
        Self {
            port: format!("{} {} 8N1", settings.port, settings.baud),
            login: match login {
                Some(role) => format!("login: {}", role.as_str()),
                None => "login: by hand".into(),
            },
            message: String::new(),
        }
    }

    fn note(&mut self, event: ConsoleEvent) {
        self.message = match event {
            ConsoleEvent::LoginRequested {
                device,
                account,
                role,
            } => format!(
                "asking for the code: {account}@{device} as {}",
                role.as_str()
            ),
            ConsoleEvent::LoginAnswered {
                device,
                account,
                role,
            } => format!("code typed: {account}@{device} as {}", role.as_str()),
            ConsoleEvent::LoginFailed { device, reason } => {
                format!("no code for {device} ({reason}): type an offline code")
            }
            ConsoleEvent::Screen | ConsoleEvent::Closed => return,
        };
    }

    fn line(&self, escaped: bool) -> String {
        let keys = if escaped {
            "C-a: x quit · b break · C-a send C-a"
        } else {
            "C-a x quit"
        };
        let mut parts = vec![self.port.as_str(), self.login.as_str()];
        if !self.message.is_empty() {
            parts.push(&self.message);
        }
        parts.push(keys);
        format!(" {}", parts.join(" │ "))
    }
}
