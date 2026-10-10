use std::collections::{HashSet, VecDeque};

use error_stack::ResultExt;
use remora_channel::{application::ChannelService, model::SshRole};
use remora_console::{
    adapter::serial::{SerialLink, SerialReader, SerialWriter},
    application::{ConsoleSession, Error, Result},
    model::{awaits_password, ConsoleEvent, ConsoleRequest, LoginPrompt},
};
use remora_context::model::ContextOverride;
use tokio::sync::mpsc;

/// No scrollback: the terminal it is drawn on keeps none of an alternate
/// screen either, and the challenge is read off what is shown.
const SCROLLBACK: usize = 0;

/// A console session: the device's bytes run through a vt100 parser into
/// the screen, and every change is looked at for a login challenge.
///
/// A challenge is answered once, whatever happens to it: a refused code
/// brings a new login, which shows a new challenge. Its code is asked for
/// in the background -- the console keeps flowing meanwhile -- and typed
/// only once the password prompt is up, so not one byte of it lands
/// anywhere else.
pub(crate) struct Session {
    parser: vt100::Parser,
    reader: Box<dyn SerialReader>,
    writer: Box<dyn SerialWriter>,
    channel: ChannelService,
    over: Option<ContextOverride>,
    login: Option<SshRole>,
    /// Challenges already seen, by text.
    seen: HashSet<String>,
    /// A code fetched for a prompt, waiting for the password prompt.
    pending: Option<(LoginPrompt, String)>,
    codes: mpsc::UnboundedSender<(LoginPrompt, std::result::Result<String, String>)>,
    fetched: mpsc::UnboundedReceiver<(LoginPrompt, std::result::Result<String, String>)>,
    /// Events to hand out before waiting for more.
    queue: VecDeque<ConsoleEvent>,
}

impl Session {
    pub(crate) fn new(link: SerialLink, channel: ChannelService, request: ConsoleRequest) -> Self {
        let (codes, fetched) = mpsc::unbounded_channel();
        Self {
            parser: vt100::Parser::new(request.rows, request.cols, SCROLLBACK),
            reader: link.reader,
            writer: link.writer,
            channel,
            over: request.over,
            login: request.login,
            seen: HashSet::new(),
            pending: None,
            codes,
            fetched,
            queue: VecDeque::new(),
        }
    }

    /// A new challenge on screen: have its code fetched.
    fn look_for_challenge(&mut self) {
        let Some(role) = self.login else { return };
        let Some(prompt) = LoginPrompt::find(&self.parser.screen().contents()) else {
            return;
        };
        if !self.seen.insert(prompt.challenge.clone()) {
            return;
        }
        self.queue.push_back(ConsoleEvent::LoginRequested {
            device: prompt.device.clone(),
            account: prompt.account.clone(),
            role,
        });
        let channel = self.channel.clone();
        let over = self.over.clone();
        let codes = self.codes.clone();
        tokio::spawn(async move {
            let code = channel
                .login_code(over.as_ref(), &prompt.device, role, &prompt.challenge)
                .await
                .map_err(|report| {
                    // The platform's own words, when it gave a reason.
                    let reason = report
                        .frames()
                        .filter_map(|frame| frame.downcast_ref::<String>())
                        .last()
                        .cloned();
                    reason.unwrap_or_else(|| report.current_context().to_string())
                });
            let _ = codes.send((prompt, code));
        });
    }

    /// Types the pending code if the password prompt is waiting for it.
    async fn answer_if_asked(&mut self) -> Result<()> {
        let Some(role) = self.login else {
            return Ok(());
        };
        if self.pending.is_none() || !awaits_password(&self.cursor_line()) {
            return Ok(());
        }
        let (prompt, code) = self.pending.take().expect("checked above");
        self.writer
            .write(format!("{code}\r").as_bytes())
            .await
            .change_context(Error::Serial)?;
        self.queue.push_back(ConsoleEvent::LoginAnswered {
            device: prompt.device,
            account: prompt.account,
            role,
        });
        Ok(())
    }

    /// The text of the row the cursor is on, up to the cursor.
    fn cursor_line(&self) -> String {
        let screen = self.parser.screen();
        let (row, col) = screen.cursor_position();
        let (_, cols) = screen.size();
        screen
            .rows(0, cols)
            .nth(row.into())
            .map(|line| line.chars().take(col.into()).collect())
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl ConsoleSession for Session {
    async fn next(&mut self) -> Result<ConsoleEvent> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(event);
            }
            tokio::select! {
                chunk = self.reader.read() => {
                    let Some(chunk) = chunk.change_context(Error::Serial)? else {
                        return Ok(ConsoleEvent::Closed);
                    };
                    self.parser.process(&chunk);
                    self.look_for_challenge();
                    self.answer_if_asked().await?;
                    return Ok(ConsoleEvent::Screen);
                }
                Some((prompt, code)) = self.fetched.recv() => match code {
                    Ok(code) => {
                        self.pending = Some((prompt, code));
                        self.answer_if_asked().await?;
                    }
                    Err(reason) => self.queue.push_back(ConsoleEvent::LoginFailed {
                        device: prompt.device,
                        reason,
                    }),
                },
            }
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write(bytes).await.change_context(Error::Serial)
    }

    async fn send_break(&mut self) -> Result<()> {
        self.writer.send_break().await.change_context(Error::Serial)
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}
