use error_stack::ResultExt;
use remora_channel::application::ChannelService;
use remora_console::{
    adapter::serial::SerialPortAdapterService,
    application::{ConsoleServiceInterface, ConsoleSession, Error, Result},
    model::ConsoleRequest,
};

use crate::session::Session;

/// The console vertical's use case: the serial port, and the channel
/// vertical's application port for the codes that answer login challenges.
pub struct ConsoleControllerImpl {
    serial: SerialPortAdapterService,
    channel: ChannelService,
}

impl ConsoleControllerImpl {
    pub fn new(serial: SerialPortAdapterService, channel: ChannelService) -> Self {
        Self { serial, channel }
    }
}

#[async_trait::async_trait]
impl ConsoleServiceInterface for ConsoleControllerImpl {
    async fn open(&self, request: ConsoleRequest) -> Result<Box<dyn ConsoleSession>> {
        let link = self
            .serial
            .open(&request.settings)
            .await
            .change_context_lazy(|| Error::Open(request.settings.port.clone()))?;
        Ok(Box::new(Session::new(link, self.channel.clone(), request)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use error_stack::Report;
    use remora_channel::{
        adapter::gateway::Channel,
        application::{self as channel, ChannelServiceInterface},
        model::{
            ExecOutcome, ExecRequest, LocalCertificateRequest, LocalSshCertificate,
            OfflineLoginCode, PreparedSsh, ScpRequest, SshRequest, SshRole,
        },
    };
    use remora_console::{
        adapter::serial::{self, SerialLink, SerialPortAdapter, SerialReader, SerialWriter},
        application::ConsoleService,
        model::{ConsoleEvent, SerialSettings},
    };
    use remora_context::model::ContextOverride;
    use remora_progress::OperationContext;
    use tokio::sync::mpsc;

    use super::*;

    /// A device at the other end of the cable: what the test feeds it says,
    /// what it was sent is kept.
    struct Cable {
        says: Mutex<Option<mpsc::UnboundedReceiver<Vec<u8>>>>,
        heard: Arc<Mutex<Vec<u8>>>,
    }

    struct Says(mpsc::UnboundedReceiver<Vec<u8>>);
    struct Hears(Arc<Mutex<Vec<u8>>>);

    #[async_trait::async_trait]
    impl SerialReader for Says {
        async fn read(&mut self) -> serial::Result<Option<Vec<u8>>> {
            Ok(self.0.recv().await)
        }
    }

    #[async_trait::async_trait]
    impl SerialWriter for Hears {
        async fn write(&mut self, bytes: &[u8]) -> serial::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        async fn send_break(&mut self) -> serial::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl SerialPortAdapter for Cable {
        async fn open(&self, settings: &SerialSettings) -> serial::Result<SerialLink> {
            let says = self
                .says
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| Report::new(serial::Error::Open(settings.port.clone())))?;
            Ok(SerialLink {
                reader: Box::new(Says(says)),
                writer: Box::new(Hears(self.heard.clone())),
            })
        }
    }

    /// The platform, for login codes only: a code that names what it was
    /// asked for; "OFFLINE" can't be reached.
    #[derive(Default, Clone)]
    struct Platform(Arc<Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl ChannelServiceInterface for Platform {
        async fn login_code(
            &self,
            _: Option<&ContextOverride>,
            device: &str,
            role: SshRole,
            challenge: &str,
        ) -> channel::Result<String> {
            self.0.lock().unwrap().push(challenge.to_owned());
            if device == "OFFLINE" {
                return Err(Report::new(channel::Error::LoginCode(device.into()))
                    .attach("failed to reach the gateway".to_string()));
            }
            Ok(format!(
                "{}-{}",
                role.as_str().to_uppercase(),
                challenge.replace('-', "")
            ))
        }

        async fn open(
            &self,
            _: Option<&ContextOverride>,
            _: &str,
            _: &str,
        ) -> channel::Result<Channel> {
            unimplemented!()
        }
        async fn prepare_ssh(&self, _: SshRequest) -> channel::Result<PreparedSsh> {
            unimplemented!()
        }
        async fn prepare_scp(&self, _: ScpRequest) -> channel::Result<PreparedSsh> {
            unimplemented!()
        }
        async fn run_ssh(&self, _: PreparedSsh) -> channel::Result<i32> {
            unimplemented!()
        }
        async fn exec(&self, _: ExecRequest, _: &OperationContext) -> channel::Result<ExecOutcome> {
            unimplemented!()
        }
        async fn certify_local(
            &self,
            _: LocalCertificateRequest,
        ) -> channel::Result<LocalSshCertificate> {
            unimplemented!()
        }
        async fn offline_login_codes(
            &self,
            _: Option<&ContextOverride>,
            _: &str,
            _: SshRole,
            _: u32,
        ) -> channel::Result<Vec<OfflineLoginCode>> {
            unimplemented!()
        }
    }

    struct Bench {
        device: mpsc::UnboundedSender<Vec<u8>>,
        heard: Arc<Mutex<Vec<u8>>>,
        asked: Arc<Mutex<Vec<String>>>,
        session: Box<dyn ConsoleSession>,
    }

    async fn bench(login: Option<SshRole>) -> Bench {
        let (device, says) = mpsc::unbounded_channel();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let platform = Platform::default();
        let asked = platform.0.clone();
        let console = ConsoleService::new(ConsoleControllerImpl::new(
            SerialPortAdapterService::new(Cable {
                says: Mutex::new(Some(says)),
                heard: heard.clone(),
            }),
            ChannelService::new(platform),
        ));
        let session = console
            .open(ConsoleRequest {
                over: None,
                settings: SerialSettings {
                    port: "/dev/ttyUSB0".into(),
                    baud: 115_200,
                },
                rows: 24,
                cols: 80,
                login,
            })
            .await
            .unwrap();
        Bench {
            device,
            heard,
            asked,
            session,
        }
    }

    const CHALLENGE: &[u8] = b"525400C0FFEE login: root\r\n\
        Remora local login: root@525400C0FFEE\r\n\
        Challenge: K7QM-3XRB\r\n\
        Type the code for this challenge (or an offline code) at the password prompt.\r\n";

    /// The events up to `last`, the screen's own left out.
    async fn until(
        session: &mut Box<dyn ConsoleSession>,
        last: fn(&ConsoleEvent) -> bool,
    ) -> Vec<ConsoleEvent> {
        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), session.next())
                .await
                .expect("the session went quiet")
                .unwrap();
            if event != ConsoleEvent::Screen {
                let done = last(&event);
                seen.push(event);
                if done {
                    return seen;
                }
            }
        }
    }

    #[tokio::test]
    async fn answers_a_challenge_at_the_password_prompt() {
        let mut bench = bench(Some(SshRole::Admin)).await;
        bench.device.send(CHALLENGE.to_vec()).unwrap();
        // The code is asked for at once, but typed only at the prompt.
        let events = until(&mut bench.session, |e| {
            matches!(e, ConsoleEvent::LoginRequested { .. })
        })
        .await;
        assert_eq!(
            events,
            [ConsoleEvent::LoginRequested {
                device: "525400C0FFEE".into(),
                account: "root".into(),
                role: SshRole::Admin,
            }]
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(bench.heard.lock().unwrap().is_empty());

        bench.device.send(b"Password: ".to_vec()).unwrap();
        until(&mut bench.session, |e| {
            matches!(e, ConsoleEvent::LoginAnswered { .. })
        })
        .await;
        assert_eq!(bench.heard.lock().unwrap().as_slice(), b"ADMIN-K7QM3XRB\r");
        assert!(bench
            .session
            .screen()
            .contents()
            .contains("Challenge: K7QM-3XRB"));

        // The same challenge still on screen is not answered again; a new
        // one is.
        bench
            .device
            .send(b"\r\nLogin incorrect\r\n".to_vec())
            .unwrap();
        bench
            .device
            .send(
                b"Remora local login: root@525400C0FFEE\r\nChallenge: AB12-CD34\r\nPassword: "
                    .to_vec(),
            )
            .unwrap();
        until(&mut bench.session, |e| {
            matches!(e, ConsoleEvent::LoginAnswered { .. })
        })
        .await;
        assert_eq!(*bench.asked.lock().unwrap(), ["K7QM-3XRB", "AB12-CD34"]);
        assert!(bench.heard.lock().unwrap().ends_with(b"ADMIN-AB12CD34\r"));
    }

    #[tokio::test]
    async fn without_a_code_the_operator_is_told_and_nothing_is_typed() {
        let mut bench = bench(Some(SshRole::User)).await;
        bench
            .device
            .send(
                b"Remora local login: root@OFFLINE\r\nChallenge: K7QM-3XRB\r\nPassword: ".to_vec(),
            )
            .unwrap();
        let events = until(&mut bench.session, |e| {
            matches!(e, ConsoleEvent::LoginFailed { .. })
        })
        .await;
        assert_eq!(
            events.last(),
            Some(&ConsoleEvent::LoginFailed {
                device: "OFFLINE".into(),
                reason: "failed to reach the gateway".into(),
            })
        );
        assert!(bench.heard.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn by_hand_nothing_is_asked_and_keys_go_through() {
        let mut bench = bench(None).await;
        bench.device.send(CHALLENGE.to_vec()).unwrap();
        bench.device.send(b"Password: ".to_vec()).unwrap();
        assert_eq!(bench.session.next().await.unwrap(), ConsoleEvent::Screen);
        assert_eq!(bench.session.next().await.unwrap(), ConsoleEvent::Screen);
        bench.session.send(b"OFF-LINE\r").await.unwrap();
        assert!(bench.asked.lock().unwrap().is_empty());
        assert_eq!(bench.heard.lock().unwrap().as_slice(), b"OFF-LINE\r");

        // The port going away ends the session.
        drop(bench.device);
        assert_eq!(bench.session.next().await.unwrap(), ConsoleEvent::Closed);
    }
}
