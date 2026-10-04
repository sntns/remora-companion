use error_stack::{Report, ResultExt};
use remora_channel::{
    adapter::gateway::{
        Channel, ChannelGatewayAdapter, ChannelReceiver, ChannelSender, Error, Result,
    },
    model::{ChannelKind, OpenedChannel, SshCertificate, SshRole},
};
use remora_context::model::ResolvedContext;
use remora_platform_grpc::{
    connect, sntns::service::remorachannel::v1 as pb, status_summary, Connection, GatewayChannel,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

type Client = pb::channel_service_client::ChannelServiceClient<GatewayChannel>;

/// The remora-channel gateway over gRPC.
///
/// One `OpenDeviceChannel` call is one channel is one connection at the
/// device: no multiplexing, raw bytes in `channel_chunk`, the client's
/// half-close is ending its request stream, the device's is an
/// `eof_response` (a server cannot end only its half of a bidi call).
pub struct GatewayAdapterImpl;

impl GatewayAdapterImpl {
    async fn client(context: &ResolvedContext) -> Result<Client> {
        let channel = connect(&Connection::for_resolved(context))
            .await
            .change_context(Error::Unreachable)?;
        // A channel can carry a whole file: lift the 4 MiB default decode
        // cap off single messages rather than fail a big chunk mid-transfer.
        Ok(Client::new(channel).max_decoding_message_size(usize::MAX))
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
        let mut client = Self::client(context).await?;

        // Small, so a slow device pushes back on stdin instead of chunks
        // piling up here: HTTP/2 flow control does the rest.
        let (requests, outgoing) = mpsc::channel(4);
        requests
            .send(pb::ChannelServiceOpenDeviceChannelRequest {
                request: Some(
                    pb::channel_service_open_device_channel_request::Request::InitialRequest(
                        pb::ChannelServiceOpenDeviceChannelInitialRequest {
                            channel_device_name: device.to_owned(),
                            channel_profile: profile.to_owned(),
                        },
                    ),
                ),
            })
            .await
            .expect("the receiver is held just below");

        let mut responses = client
            .open_device_channel(ReceiverStream::new(outgoing))
            .await
            .map_err(classify)?
            .into_inner();

        // Exactly one opened_response comes first; nothing may be written
        // before it, since bytes sent earlier have nowhere to go.
        let opened = match responses.message().await.map_err(classify)? {
            Some(pb::ChannelServiceOpenDeviceChannelResponse {
                response:
                    Some(pb::channel_service_open_device_channel_response::Response::OpenedResponse(
                        opened,
                    )),
            }) => opened,
            Some(_) => {
                return Err(Report::new(Error::Protocol)
                    .attach("the first message of a channel was not its opening"))
            }
            None => {
                return Err(Report::new(Error::Protocol)
                    .attach("the gateway ended the channel before opening it"))
            }
        };
        let kind = match opened.channel_kind.as_str() {
            "stream" | "" => ChannelKind::Stream,
            "datagram" => ChannelKind::Datagram,
            other => {
                return Err(
                    Report::new(Error::Protocol).attach(format!("unknown channel kind {other:?}"))
                )
            }
        };

        Ok(Channel {
            opened: OpenedChannel {
                device_urn: opened
                    .channel_device_reference
                    .map(|reference| reference.urn)
                    .unwrap_or_default(),
                kind,
            },
            sender: Box::new(GrpcSender(Some(requests))),
            receiver: Box::new(GrpcReceiver {
                responses,
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
        let signed = Self::client(context)
            .await?
            .sign_device_ssh_certificate(pb::ChannelServiceSignDeviceSshCertificateRequest {
                channel_device_name: device.to_owned(),
                ssh_public_key: public_key.to_owned(),
                ssh_role: role.as_str().to_owned(),
            })
            .await
            .map_err(classify)?
            .into_inner();
        Ok(SshCertificate {
            certificate: signed.ssh_certificate,
            user: signed.ssh_user,
            host_key_alias: signed.ssh_host_key_alias,
            known_hosts: signed.ssh_known_hosts,
        })
    }
}

/// `None` once write-closed: dropping the request sender is what ends the
/// request stream, which the server reads as the client's half-close.
struct GrpcSender(Option<mpsc::Sender<pb::ChannelServiceOpenDeviceChannelRequest>>);

#[async_trait::async_trait]
impl ChannelSender for GrpcSender {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()> {
        let Some(requests) = &self.0 else {
            return Err(Report::new(Error::Ended).attach("write after close_write"));
        };
        requests
            .send(pb::ChannelServiceOpenDeviceChannelRequest {
                request: Some(
                    pb::channel_service_open_device_channel_request::Request::SubsequentRequest(
                        pb::ChannelServiceOpenDeviceChannelSubsequentRequest {
                            channel_chunk: chunk,
                        },
                    ),
                ),
            })
            .await
            .map_err(|_| Report::new(Error::Ended))
    }

    async fn close_write(&mut self) -> Result<()> {
        self.0 = None;
        Ok(())
    }
}

struct GrpcReceiver {
    responses: tonic::Streaming<pb::ChannelServiceOpenDeviceChannelResponse>,
    ended: bool,
}

#[async_trait::async_trait]
impl ChannelReceiver for GrpcReceiver {
    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        use pb::channel_service_open_device_channel_response::Response;
        while !self.ended {
            // Mid-channel, any failure is the gateway ending it: an
            // `Unavailable` here is a device or replica going away, not a
            // gateway out of reach as it would be when opening.
            let message = self
                .responses
                .message()
                .await
                .map_err(|status| Report::new(Error::Ended).attach(status_summary(&status)))?;
            match message {
                None => self.ended = true,
                Some(message) => match message.response {
                    Some(Response::ChunkResponse(chunk)) => return Ok(Some(chunk.channel_chunk)),
                    // The device half-closed; this end may still write.
                    Some(Response::EofResponse(_)) => self.ended = true,
                    // A message this version doesn't know (or a second
                    // opening): skipped rather than turned into bytes.
                    Some(Response::OpenedResponse(_)) | None => continue,
                },
            }
        }
        Ok(None)
    }
}

/// Turns a gRPC status into this port's error, keeping the server's own
/// message: for a refused channel it is the device's words, verbatim
/// ("the device refused the channel: remote access disabled on this device").
fn classify(status: tonic::Status) -> Report<Error> {
    use tonic::Code;
    let error = match status.code() {
        Code::Unauthenticated => Error::Unauthenticated,
        Code::PermissionDenied => Error::PermissionDenied,
        Code::NotFound => Error::NotFound,
        Code::Unavailable => Error::Unreachable,
        Code::FailedPrecondition if status.message().contains("no session") => Error::NotConnected,
        _ => Error::Channel,
    };
    Report::new(error).attach(status_summary(&status))
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;

    use remora_context::model::{Context, Credentials, Endpoint, Secret, Selection, Tls};
    use remora_platform_grpc::sntns::service::v1::ResourceReference;
    use tokio_stream::{Stream, StreamExt};
    use tonic::{Request, Response, Status, Streaming};

    use super::*;

    /// An in-process gateway whose devices echo back what they receive,
    /// upper-cased, then half-close once the client has.
    struct EchoGateway;

    type Responses = Pin<
        Box<
            dyn Stream<
                    Item = std::result::Result<pb::ChannelServiceOpenDeviceChannelResponse, Status>,
                > + Send,
        >,
    >;

    fn response(
        response: pb::channel_service_open_device_channel_response::Response,
    ) -> std::result::Result<pb::ChannelServiceOpenDeviceChannelResponse, Status> {
        Ok(pb::ChannelServiceOpenDeviceChannelResponse {
            response: Some(response),
        })
    }

    #[tonic::async_trait]
    impl pb::channel_service_server::ChannelService for EchoGateway {
        type OpenDeviceChannelStream = Responses;

        async fn open_device_channel(
            &self,
            request: Request<Streaming<pb::ChannelServiceOpenDeviceChannelRequest>>,
        ) -> std::result::Result<Response<Responses>, Status> {
            use pb::channel_service_open_device_channel_request::Request as In;
            use pb::channel_service_open_device_channel_response::Response as Out;

            let mut incoming = request.into_inner();
            let Some(Ok(pb::ChannelServiceOpenDeviceChannelRequest {
                request: Some(In::InitialRequest(initial)),
            })) = incoming.next().await
            else {
                return Err(Status::invalid_argument("no initial request"));
            };
            if initial.channel_device_name == "OFF" {
                return Err(Status::permission_denied(
                    "the device refused the channel: remote access disabled on this device",
                ));
            }
            let urn = format!("urn:remora:device:{}", initial.channel_device_name);
            let outgoing = async_stream(incoming, urn, |chunk| {
                Out::ChunkResponse(pb::ChannelServiceOpenDeviceChannelChunkResponse {
                    channel_chunk: chunk.to_ascii_uppercase(),
                })
            });
            Ok(Response::new(outgoing))
        }

        async fn sign_device_ssh_certificate(
            &self,
            request: Request<pb::ChannelServiceSignDeviceSshCertificateRequest>,
        ) -> std::result::Result<Response<pb::ChannelServiceSignDeviceSshCertificateResponse>, Status>
        {
            let request = request.into_inner();
            Ok(Response::new(
                pb::ChannelServiceSignDeviceSshCertificateResponse {
                    ssh_certificate: format!("cert-for {}", request.ssh_public_key),
                    ssh_user: request.ssh_role,
                    ssh_host_key_alias: request.channel_device_name.to_lowercase(),
                    ssh_known_hosts: "@cert-authority * ssh-ed25519 AAAA".into(),
                },
            ))
        }
    }

    /// opened, then one transformed chunk per incoming chunk, then eof once
    /// the client half-closes.
    fn async_stream(
        mut incoming: Streaming<pb::ChannelServiceOpenDeviceChannelRequest>,
        urn: String,
        transform: fn(&[u8]) -> pb::channel_service_open_device_channel_response::Response,
    ) -> Responses {
        use pb::channel_service_open_device_channel_request::Request as In;
        use pb::channel_service_open_device_channel_response::Response as Out;
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(response(Out::OpenedResponse(
                    pb::ChannelServiceOpenDeviceChannelOpenedResponse {
                        channel_device_reference: Some(ResourceReference {
                            urn,
                            ..Default::default()
                        }),
                        channel_kind: "stream".into(),
                    },
                )))
                .await;
            while let Some(Ok(message)) = incoming.next().await {
                if let Some(In::SubsequentRequest(chunk)) = message.request {
                    let _ = tx.send(response(transform(&chunk.channel_chunk))).await;
                }
            }
            let _ = tx
                .send(response(Out::EofResponse(
                    pb::ChannelServiceOpenDeviceChannelEofResponse {},
                )))
                .await;
            // Keep the call open a moment after eof, as a real gateway
            // does while the other direction drains.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        });
        Box::pin(ReceiverStream::new(rx))
    }

    async fn serve() -> ResolvedContext {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(pb::channel_service_server::ChannelServiceServer::new(
                    EchoGateway,
                ))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        ResolvedContext {
            context: Context {
                name: "local".into(),
                description: None,
                endpoint: Endpoint {
                    address,
                    tls: Tls {
                        disabled: true,
                        ..Tls::default()
                    },
                },
                roles: Default::default(),
                assumed_role: None,
                login: None,
            },
            credentials: Credentials {
                secret: Secret::AccessKey { token: "t".into() },
            },
            selection: Selection::Flag,
            role: None,
        }
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

    #[tokio::test]
    async fn a_refusal_keeps_the_devices_words() {
        let context = serve().await;
        let report = match GatewayAdapterImpl.open(&context, "OFF", "ssh").await {
            Ok(_) => panic!("the device refused"),
            Err(report) => report,
        };
        assert!(matches!(report.current_context(), Error::PermissionDenied));
        assert!(format!("{report:?}").contains("remote access disabled on this device"));
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
}
