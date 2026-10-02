use std::time::Duration;

use error_stack::{Report, ResultExt};
use remora_channel::adapter::gateway::Channel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{Error, Result};

/// How long a channel still gets to finish after a hangup: its last bytes
/// forwarded, its end of input said, the device's end read back.
const HANGUP_GRACE: Duration = Duration::from_secs(5);

const BUFFER: usize = 32 * 1024;

/// Joins a stream channel to this process's stdin and stdout until both
/// directions have ended.
///
/// The end of stdin is a half-close, not the end of the channel: the device
/// reads its end of input and still gets to answer. And a hangup -- how ssh
/// ends its ProxyCommand: last packets written, pipe closed, then SIGHUP --
/// doesn't end this process at once, or the device would see the channel
/// cut instead of ended; the relay gets [`HANGUP_GRACE`] to finish. A write
/// to a stdout nobody reads anymore (EPIPE: ssh has gone) is a normal end.
/// Rust ignores SIGPIPE already, so that arrives as an error, not a death.
pub(crate) async fn stdio(channel: Channel, device: &str) -> Result<()> {
    let Channel {
        mut sender,
        mut receiver,
        ..
    } = channel;
    let lost = || Error::Relay(device.to_owned());

    let upstream = async move {
        let mut stdin = tokio::io::stdin();
        let mut buffer = vec![0; BUFFER];
        loop {
            let read = stdin.read(&mut buffer).await.change_context_lazy(lost)?;
            if read == 0 {
                return sender.close_write().await.change_context_lazy(lost);
            }
            sender
                .send(buffer[..read].to_vec())
                .await
                .change_context_lazy(lost)?;
        }
    };

    let downstream = async move {
        let mut stdout = tokio::io::stdout();
        while let Some(chunk) = receiver.recv().await.change_context_lazy(lost)? {
            let written = async {
                stdout.write_all(&chunk).await?;
                stdout.flush().await
            };
            match written.await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                Err(e) => return Err(Report::new(e).change_context(lost())),
            }
        }
        Ok::<_, Report<Error>>(())
    };

    let both = both_directions(upstream, downstream);
    tokio::pin!(both);

    tokio::select! {
        result = &mut both => return result,
        () = hangup() => {}
    }
    tokio::time::timeout(HANGUP_GRACE, both)
        .await
        .unwrap_or_else(|_| Err(Report::new(Error::HangupGrace(HANGUP_GRACE.as_secs()))))
}

/// How long the other direction gets to explain a failure: when one
/// direction fails, the other usually fails too, with the better reason.
const EXPLAIN: Duration = Duration::from_secs(1);

/// Both directions must end; the first failure ends it all -- but reported
/// as the device's side tells it when it can. A dying gateway stream shows
/// up on the sending side as a bare "the gateway ended the channel", while
/// the receiving side gets the status saying why; so a failure from the
/// sending side waits [`EXPLAIN`] for the receiving side's.
async fn both_directions(
    upstream: impl std::future::Future<Output = Result<()>>,
    downstream: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    tokio::pin!(upstream);
    tokio::pin!(downstream);
    let (mut up_done, mut down_done) = (false, false);
    loop {
        tokio::select! {
            result = &mut upstream, if !up_done => match result {
                Ok(()) => up_done = true,
                Err(sending) => {
                    if down_done {
                        return Err(sending);
                    }
                    return match tokio::time::timeout(EXPLAIN, &mut downstream).await {
                        Ok(Err(receiving)) => Err(receiving),
                        _ => Err(sending),
                    };
                }
            },
            result = &mut downstream, if !down_done => match result {
                Ok(()) => down_done = true,
                Err(receiving) => return Err(receiving),
            },
        }
        if up_done && down_done {
            return Ok(());
        }
    }
}

#[cfg(unix)]
async fn hangup() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::hangup()) {
        Ok(mut hangup) => {
            hangup.recv().await;
        }
        // No handler, no hangup to wait for: the default action applies.
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn hangup() {
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(message: &'static str) -> Report<Error> {
        Report::new(Error::Relay("DEV".into())).attach(message)
    }

    #[tokio::test]
    async fn a_sending_failure_waits_for_the_receiving_sides_reason() {
        let upstream = async { Err(failure("the gateway ended the channel")) };
        let downstream = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err(failure("Unavailable: the device disconnected"))
        };
        let report = both_directions(upstream, downstream).await.unwrap_err();
        assert!(format!("{report:?}").contains("the device disconnected"));
    }

    #[tokio::test]
    async fn a_sending_failure_stands_when_the_other_side_has_nothing_to_add() {
        let upstream = async { Err(failure("the gateway ended the channel")) };
        let downstream = std::future::pending::<Result<()>>();
        let report = both_directions(upstream, downstream).await.unwrap_err();
        assert!(format!("{report:?}").contains("the gateway ended the channel"));
    }

    #[tokio::test]
    async fn both_directions_must_end() {
        let upstream = async { Ok(()) };
        let downstream = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        };
        both_directions(upstream, downstream).await.unwrap();
    }
}
