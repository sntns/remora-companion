use error_stack::{Report, ResultExt};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use remora_channel::{
    adapter::gateway::{
        Channel, ChannelGatewayAdapter, ChannelReceiver, ChannelSender, Error, Result,
    },
    model::{
        ChannelKind, LocalSshCertificate, LocalSshDevice, OfflineLoginCode, OpenedChannel,
        SshCertificate, SshRole,
    },
};
use remora_context::model::ResolvedContext;
use remora_platform_http::{
    model::{self, channel, time_of},
    status_summary, Code, Connection, Gateway, Message, Status, WebSocket,
};

/// The remora-channel gateway: a channel over its WebSocket, everything
/// else over its REST API.
///
/// One WebSocket is one channel is one connection at the device: no
/// multiplexing, raw bytes in binary messages, a half-close (either way) a
/// text `{"type":"eof"}`. Everything the platform decides -- the device's
/// refusal included -- comes before the upgrade, as a plain HTTP status.
pub struct GatewayAdapterImpl;

/// What one message of a stream channel carries at most: well under the
/// gateway's 1 MiB read limit, so that a big write is split rather than
/// refused (a datagram is always one message).
const STREAM_MESSAGE: usize = 64 * 1024;

impl GatewayAdapterImpl {
    async fn gateway(context: &ResolvedContext) -> Result<Gateway> {
        Gateway::new(&Connection::for_resolved(context))
            .await
            .change_context(Error::Unreachable)
    }
}

#[async_trait::async_trait]
impl ChannelGatewayAdapter for GatewayAdapterImpl {
    async fn open(
        &self,
        context: &ResolvedContext,
        device: &str,
        profile: &str,
    ) -> Result<Channel> {
        let gateway = Self::gateway(context).await?;
        let url = gateway.url([
            "remora", "channel", "v1", "device", device, "channel", profile,
        ]);
        // The upgrade is the device's yes: nothing may be written before
        // it, since bytes sent earlier have nowhere to go.
        let (socket, headers) = gateway
            .websocket(url, channel::SUBPROTOCOL)
            .await
            .map_err(classify)?;
        let header = |name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let kind = match header(channel::KIND_HEADER).as_str() {
            "stream" | "" => ChannelKind::Stream,
            "datagram" => ChannelKind::Datagram,
            other => {
                return Err(
                    Report::new(Error::Protocol).attach(format!("unknown channel kind {other:?}"))
                )
            }
        };

        let (sink, stream) = socket.split();
        Ok(Channel {
            opened: OpenedChannel {
                device_urn: header(channel::DEVICE_HEADER),
                kind,
            },
            sender: Box::new(WebSocketSender {
                sink,
                kind,
                closed: false,
            }),
            receiver: Box::new(WebSocketReceiver {
                stream,
                ended: false,
            }),
        })
    }

    async fn sign_ssh_certificate(
        &self,
        context: &ResolvedContext,
        device: &str,
        public_key: &str,
        role: SshRole,
    ) -> Result<SshCertificate> {
        let gateway = Self::gateway(context).await?;
        let signed: model::SignDeviceSshCertificateResponse = gateway
            .post(
                gateway.url([
                    "remora",
                    "channel",
                    "v1",
                    "device",
                    device,
                    "ssh-certificate",
                ]),
                &model::SignDeviceSshCertificateRequest {
                    ssh_public_key: public_key.to_owned(),
                    ssh_role: role.as_str().to_owned(),
                },
            )
            .await
            .map_err(classify)?;
        Ok(SshCertificate {
            certificate: signed.ssh_certificate,
            user: signed.ssh_user,
            host_key_alias: signed.ssh_host_key_alias,
            known_hosts: signed.ssh_known_hosts,
        })
    }

    async fn sign_local_ssh_certificate(
        &self,
        context: &ResolvedContext,
        devices: &[String],
        public_key: &str,
        role: SshRole,
        validity_hours: u32,
    ) -> Result<LocalSshCertificate> {
        let gateway = Self::gateway(context).await?;
        let signed: model::SignDeviceLocalSshCertificateResponse = gateway
            .post(
                gateway.url(["remora", "channel", "v1", "local-ssh-certificate"]),
                &model::SignDeviceLocalSshCertificateRequest {
                    channel_device_names: devices.to_vec(),
                    ssh_public_key: public_key.to_owned(),
                    ssh_role: role.as_str().to_owned(),
                    validity_hours: i32::try_from(validity_hours).unwrap_or(i32::MAX),
                },
            )
            .await
            .map_err(classify)?;
        Ok(LocalSshCertificate {
            certificate: signed.ssh_certificate,
            known_hosts: signed.ssh_known_hosts,
            devices: signed
                .ssh_devices
                .into_iter()
                .map(|device| LocalSshDevice {
                    device: device.channel_device_name,
                    user: device.ssh_user,
                    host_key_alias: device.ssh_host_key_alias,
                })
                .collect(),
            valid_before: time_of(signed.valid_before.as_deref()),
        })
    }

    async fn sign_login_challenge(
        &self,
        context: &ResolvedContext,
        device: &str,
        role: SshRole,
        challenge: &str,
    ) -> Result<String> {
        let gateway = Self::gateway(context).await?;
        let signed: model::SignDeviceLoginChallengeResponse = gateway
            .post(
                gateway.url(["remora", "channel", "v1", "device", device, "login-code"]),
                &model::SignDeviceLoginChallengeRequest {
                    ssh_role: role.as_str().to_owned(),
                    challenge: challenge.to_owned(),
                },
            )
            .await
            .map_err(classify)?;
        Ok(signed.login_code)
    }

    async fn issue_offline_login_codes(
        &self,
        context: &ResolvedContext,
        device: &str,
        role: SshRole,
        count: u32,
    ) -> Result<Vec<OfflineLoginCode>> {
        let gateway = Self::gateway(context).await?;
        let issued: model::IssueDeviceOfflineLoginCodesResponse = gateway
            .post(
                gateway.url([
                    "remora",
                    "channel",
                    "v1",
                    "device",
                    device,
                    "offline-login-codes",
                ]),
                &model::IssueDeviceOfflineLoginCodesRequest {
                    ssh_role: role.as_str().to_owned(),
                    count,
                },
            )
            .await
            .map_err(classify)?;
        Ok(issued
            .login_codes
            .into_iter()
            .map(|code| OfflineLoginCode {
                index: code.index,
                code: code.login_code,
            })
            .collect())
    }
}

struct WebSocketSender {
    sink: SplitSink<WebSocket, Message>,
    kind: ChannelKind,
    /// Once our eof is sent, nothing more may be.
    closed: bool,
}

impl WebSocketSender {
    async fn push(&mut self, message: Message) -> Result<()> {
        self.sink
            .send(message)
            .await
            .map_err(|error| Report::new(Error::Ended).attach(error.to_string()))
    }
}

#[async_trait::async_trait]
impl ChannelSender for WebSocketSender {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()> {
        if self.closed {
            return Err(Report::new(Error::Ended).attach("write after close_write"));
        }
        match self.kind {
            ChannelKind::Datagram => self.push(Message::Binary(chunk.into())).await,
            ChannelKind::Stream => {
                for piece in chunk.chunks(STREAM_MESSAGE) {
                    self.push(Message::Binary(piece.to_vec().into())).await?;
                }
                Ok(())
            }
        }
    }

    /// Our half-close; the device may still answer, and the socket stays
    /// open for it (and for the gateway's pings) until the gateway ends it.
    async fn close_write(&mut self) -> Result<()> {
        if !self.closed {
            self.closed = true;
            self.push(Message::Text(channel::EOF.into())).await?;
        }
        Ok(())
    }
}

struct WebSocketReceiver {
    stream: SplitStream<WebSocket>,
    ended: bool,
}

#[async_trait::async_trait]
impl ChannelReceiver for WebSocketReceiver {
    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        while !self.ended {
            // Reading is also what answers the gateway's pings.
            let message = match self.stream.next().await {
                None => {
                    self.ended = true;
                    continue;
                }
                // Mid-channel, any failure is the gateway ending it: a
                // device or replica going away, the connection cut.
                Some(Err(error)) => {
                    self.ended = true;
                    return Err(Report::new(Error::Ended).attach(error.to_string()));
                }
                Some(Ok(message)) => message,
            };
            match message {
                Message::Binary(bytes) => return Ok(Some(bytes.to_vec())),
                // The device half-closed; this end may still write.
                Message::Text(text) => {
                    let control = serde_json::from_str::<channel::Control>(text.as_str());
                    if matches!(control, Ok(control) if control.kind == "eof") {
                        self.ended = true;
                    }
                    // Anything else is a message this version doesn't
                    // know: skipped rather than turned into bytes.
                }
                // A normal close is both directions done; any other is
                // the gateway's failure -- a device or replica going away,
                // what gRPC called Unavailable -- its reason the
                // platform's words.
                Message::Close(frame) => {
                    self.ended = true;
                    if let Some(frame) =
                        frame.filter(|frame| !matches!(u16::from(frame.code), 1000 | 1001))
                    {
                        let reason = match frame.reason.to_string() {
                            reason if reason.is_empty() => {
                                format!("closed with code {}", u16::from(frame.code))
                            }
                            reason => reason,
                        };
                        let status = Status::new(Code::Unavailable, reason);
                        return Err(Report::new(Error::Ended).attach(status_summary(&status)));
                    }
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Ok(None)
    }
}

/// Turns a refusal into this port's error, keeping the server's own
/// message: for a refused channel it is the device's words, verbatim
/// ("the device refused the channel: remote access disabled on this device").
fn classify(status: Status) -> Report<Error> {
    let error = match status.code {
        Code::Unauthenticated => Error::Unauthenticated,
        Code::PermissionDenied => Error::PermissionDenied,
        Code::NotFound => Error::NotFound,
        Code::Unavailable => Error::Unreachable,
        // A channel's refusals are plain HTTP: a device not connected is a
        // 400 like a bad profile, told apart by the platform's words.
        _ if status.message.contains("no session") => Error::NotConnected,
        Code::FailedPrecondition => Error::FailedPrecondition,
        Code::InvalidArgument => Error::InvalidArgument,
        _ => Error::Channel,
    };
    Report::new(error).attach(status_summary(&status))
}

#[cfg(test)]
// The fake gateway's handlers answer `Result<_, Response>`, axum's own
// idiom for a refusal.
#[allow(clippy::result_large_err)]
mod tests {
    use std::time::{Duration, SystemTime};

    use remora_platform_http::test_gateway::{
        accept_channel,
        axum::{
            extract::{
                ws::{
                    CloseFrame, Message as WsMessage, WebSocket as ServerSocket, WebSocketUpgrade,
                },
                Path,
            },
            response::Response,
            routing::{get, post},
            Json, Router,
        },
        context, rpc_error, serve_router, text_error,
    };

    use super::*;

    /// A device that echoes back what it receives, upper-cased, then
    /// half-closes once the client has, and the gateway ends the channel.
    async fn echo(mut socket: ServerSocket) {
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                WsMessage::Binary(bytes) => {
                    let upper = bytes.to_ascii_uppercase();
                    if socket.send(WsMessage::Binary(upper.into())).await.is_err() {
                        return;
                    }
                }
                WsMessage::Text(text) if text.as_str() == channel::EOF => break,
                _ => {}
            }
        }
        let _ = socket.send(WsMessage::Text(channel::EOF.into())).await;
        // Keep the socket open a moment after eof, as a real gateway does
        // while the other direction drains.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = socket
            .send(WsMessage::Close(Some(CloseFrame {
                code: 1000,
                reason: "".into(),
            })))
            .await;
    }

    /// Opens, then the gateway fails it once the client wrote anything.
    async fn drop_after_first(mut socket: ServerSocket) {
        let _ = socket.recv().await;
        let _ = socket
            .send(WsMessage::Close(Some(CloseFrame {
                code: 1011,
                reason: "the device went away".into(),
            })))
            .await;
        // The closing handshake, ended: dropped with the client's data
        // unread, the connection would be reset, and Windows discards the
        // close frame with what it received but not yet read.
        let drain = async { while let Some(Ok(_)) = socket.recv().await {} };
        let _ = tokio::time::timeout(Duration::from_secs(5), drain).await;
    }

    async fn open(
        Path((device, _profile)): Path<(String, String)>,
        upgrade: WebSocketUpgrade,
    ) -> Response {
        let urn = format!("urn:remora:device:{device}");
        match device.as_str() {
            "OFF" => text_error(
                Code::PermissionDenied,
                "the device refused the channel: remote access disabled on this device",
            ),
            "AWAY" => text_error(
                Code::FailedPrecondition,
                "no session is open for this device",
            ),
            "DROP" => accept_channel(upgrade, "stream", &urn, drop_after_first),
            "DGRAM" => accept_channel(upgrade, "datagram", &urn, echo),
            _ => accept_channel(upgrade, "stream", &urn, echo),
        }
    }

    async fn ssh_certificate(
        Path(device): Path<String>,
        Json(request): Json<model::SignDeviceSshCertificateRequest>,
    ) -> Json<model::SignDeviceSshCertificateResponse> {
        Json(model::SignDeviceSshCertificateResponse {
            ssh_certificate: format!("cert-for {}", request.ssh_public_key),
            ssh_user: request.ssh_role,
            ssh_host_key_alias: device.to_lowercase(),
            ssh_known_hosts: "@cert-authority * ssh-ed25519 AAAA".into(),
        })
    }

    /// One entry per device, valid until 2026-10-09 12:00:00Z.
    async fn local_ssh_certificate(
        Json(request): Json<model::SignDeviceLocalSshCertificateRequest>,
    ) -> std::result::Result<Json<model::SignDeviceLocalSshCertificateResponse>, Response> {
        if request.validity_hours > 72 {
            return Err(rpc_error(
                Code::InvalidArgument,
                "validity above the account's maximum",
            ));
        }
        Ok(Json(model::SignDeviceLocalSshCertificateResponse {
            ssh_certificate: format!("local-{} {}", request.ssh_role, request.ssh_public_key),
            ssh_known_hosts: "@cert-authority *.devices.sentiens ssh-ed25519 AAAA".into(),
            ssh_devices: request
                .channel_device_names
                .iter()
                .map(|name| model::LocalSshDevice {
                    channel_device_name: name.clone(),
                    ssh_user: "root".into(),
                    ssh_host_key_alias: format!("{}.devices.sentiens", name.to_lowercase()),
                })
                .collect(),
            valid_before: Some(model::timestamp(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_547_200),
            )),
        }))
    }

    async fn login_code(
        Json(request): Json<model::SignDeviceLoginChallengeRequest>,
    ) -> std::result::Result<Json<model::SignDeviceLoginChallengeResponse>, Response> {
        if request.ssh_role == "admin" {
            return Err(rpc_error(Code::FailedPrecondition, "unmapped role"));
        }
        Ok(Json(model::SignDeviceLoginChallengeResponse {
            login_code: format!("{}:{}", request.ssh_role, request.challenge),
        }))
    }

    async fn offline_login_codes(
        Json(request): Json<model::IssueDeviceOfflineLoginCodesRequest>,
    ) -> Json<model::IssueDeviceOfflineLoginCodesResponse> {
        Json(model::IssueDeviceOfflineLoginCodesResponse {
            login_codes: (40..40 + request.count)
                .map(|index| model::OfflineLoginCode {
                    index,
                    login_code: format!("C{index}"),
                })
                .collect(),
        })
    }

    async fn serve() -> ResolvedContext {
        let device = "/remora/channel/v1/device/{device}";
        let address = serve_router(
            Router::new()
                .route(&format!("{device}/channel/{{profile}}"), get(open))
                .route(&format!("{device}/ssh-certificate"), post(ssh_certificate))
                .route(&format!("{device}/login-code"), post(login_code))
                .route(
                    &format!("{device}/offline-login-codes"),
                    post(offline_login_codes),
                )
                .route(
                    "/remora/channel/v1/local-ssh-certificate",
                    post(local_ssh_certificate),
                ),
        )
        .await;
        context(address, "t")
    }

    #[tokio::test]
    async fn carries_bytes_both_ways_and_half_closes() {
        let context = serve().await;
        let mut channel = GatewayAdapterImpl
            .open(&context, "525400C0FFEE", "ssh")
            .await
            .unwrap();
        assert_eq!(channel.opened.device_urn, "urn:remora:device:525400C0FFEE");
        assert_eq!(channel.opened.kind, ChannelKind::Stream);

        channel.sender.send(b"hello".to_vec()).await.unwrap();
        assert_eq!(
            channel.receiver.recv().await.unwrap(),
            Some(b"HELLO".to_vec())
        );
        channel.sender.send(b"again".to_vec()).await.unwrap();
        assert_eq!(
            channel.receiver.recv().await.unwrap(),
            Some(b"AGAIN".to_vec())
        );

        // Our half-close reaches the device, whose eof comes back as None.
        channel.sender.close_write().await.unwrap();
        assert_eq!(channel.receiver.recv().await.unwrap(), None);
        assert_eq!(channel.receiver.recv().await.unwrap(), None);
        assert!(channel.sender.send(b"late".to_vec()).await.is_err());
    }

    /// A stream write bigger than the gateway reads in one message arrives
    /// whole, split; a datagram stays one message.
    #[tokio::test]
    async fn splits_a_big_stream_write_but_never_a_datagram() {
        let context = serve().await;
        let mut channel = GatewayAdapterImpl
            .open(&context, "BIG", "ssh")
            .await
            .unwrap();
        let big = vec![b'x'; 3 * STREAM_MESSAGE + 7];
        channel.sender.send(big.clone()).await.unwrap();
        let mut echoed = Vec::new();
        while echoed.len() < big.len() {
            echoed.extend(channel.receiver.recv().await.unwrap().unwrap());
        }
        assert_eq!(echoed, big.to_ascii_uppercase());

        let mut datagrams = GatewayAdapterImpl
            .open(&context, "DGRAM", "vnc")
            .await
            .unwrap();
        assert_eq!(datagrams.opened.kind, ChannelKind::Datagram);
        let datagram = vec![b'y'; STREAM_MESSAGE + 1];
        datagrams.sender.send(datagram.clone()).await.unwrap();
        assert_eq!(
            datagrams.receiver.recv().await.unwrap().unwrap().len(),
            datagram.len()
        );
    }

    #[tokio::test]
    async fn a_channel_failed_mid_way_says_why() {
        let context = serve().await;
        let mut channel = GatewayAdapterImpl
            .open(&context, "DROP", "ssh")
            .await
            .unwrap();
        channel.sender.send(b"keystrokes".to_vec()).await.unwrap();
        let report = channel.receiver.recv().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Ended));
        assert!(format!("{report:?}").contains("Unavailable: the device went away"));
    }

    #[tokio::test]
    async fn a_refusal_keeps_the_devices_words() {
        let context = serve().await;
        let report = match GatewayAdapterImpl.open(&context, "OFF", "ssh").await {
            Ok(_) => panic!("the device refused"),
            Err(report) => report,
        };
        assert!(matches!(report.current_context(), Error::PermissionDenied));
        assert!(format!("{report:?}").contains("remote access disabled on this device"));

        let report = match GatewayAdapterImpl.open(&context, "AWAY", "ssh").await {
            Ok(_) => panic!("the device is not connected"),
            Err(report) => report,
        };
        assert!(matches!(report.current_context(), Error::NotConnected));
    }

    #[tokio::test]
    async fn certifies_a_key_for_a_role() {
        let context = serve().await;
        let signed = GatewayAdapterImpl
            .sign_ssh_certificate(&context, "ABC", "ssh-ed25519 KEY", SshRole::Admin)
            .await
            .unwrap();
        assert_eq!(signed.certificate, "cert-for ssh-ed25519 KEY");
        assert_eq!(signed.user, "admin");
        assert_eq!(signed.host_key_alias, "abc");
    }

    #[tokio::test]
    async fn certifies_a_key_for_local_ssh_to_several_devices() {
        let context = serve().await;
        let devices = vec!["DEV1".to_owned(), "DEV2".to_owned()];
        let issued = GatewayAdapterImpl
            .sign_local_ssh_certificate(&context, &devices, "ssh-ed25519 KEY", SshRole::Admin, 8)
            .await
            .unwrap();
        assert_eq!(issued.certificate, "local-admin ssh-ed25519 KEY");
        assert_eq!(issued.devices.len(), 2);
        assert_eq!(issued.devices[1].device, "DEV2");
        assert_eq!(issued.devices[1].host_key_alias, "dev2.devices.sentiens");
        assert_eq!(
            issued.valid_before,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_547_200))
        );

        let report = GatewayAdapterImpl
            .sign_local_ssh_certificate(&context, &devices, "ssh-ed25519 KEY", SshRole::User, 100)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::InvalidArgument));
    }

    #[tokio::test]
    async fn signs_challenges_and_issues_offline_codes() {
        let context = serve().await;
        assert_eq!(
            GatewayAdapterImpl
                .sign_login_challenge(&context, "DEV1", SshRole::User, "K7QM-3XRB")
                .await
                .unwrap(),
            "user:K7QM-3XRB"
        );
        let report = GatewayAdapterImpl
            .sign_login_challenge(&context, "DEV1", SshRole::Admin, "K7QM-3XRB")
            .await
            .unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::FailedPrecondition
        ));
        assert!(format!("{report:?}").contains("unmapped role"));

        let codes = GatewayAdapterImpl
            .issue_offline_login_codes(&context, "DEV1", SshRole::Admin, 2)
            .await
            .unwrap();
        assert_eq!(
            codes,
            [
                OfflineLoginCode {
                    index: 40,
                    code: "C40".into()
                },
                OfflineLoginCode {
                    index: 41,
                    code: "C41".into()
                },
            ]
        );
    }
}
