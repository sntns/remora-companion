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
pub(crate) async fn stdio(channel: Channel) -> Result<()> {
    let Channel {
        mut sender,
        mut receiver,
        ..
    } = channel;

    let upstream = async move {
        let mut stdin = tokio::io::stdin();
        let mut buffer = vec![0; BUFFER];
        loop {
            let read = stdin.read(&mut buffer).await.change_context(Error::Relay)?;
            if read == 0 {
                return sender.close_write().await.change_context(Error::Relay);
            }
            sender
                .send(buffer[..read].to_vec())
                .await
                .change_context(Error::Relay)?;
        }
    };

    let downstream = async move {
        let mut stdout = tokio::io::stdout();
        while let Some(chunk) = receiver.recv().await.change_context(Error::Relay)? {
            let written = async {
                stdout.write_all(&chunk).await?;
                stdout.flush().await
            };
            match written.await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                Err(e) => return Err(Report::new(e).change_context(Error::Relay)),
            }
        }
        Ok::<_, Report<Error>>(())
    };

    // Both directions must end; the first failure ends it all.
    let both = async {
        tokio::try_join!(upstream, downstream)?;
        Ok::<_, Report<Error>>(())
    };
    tokio::pin!(both);

    tokio::select! {
        result = &mut both => return result,
        () = hangup() => {}
    }
    tokio::time::timeout(HANGUP_GRACE, both)
        .await
        .unwrap_or_else(|_| Err(Report::new(Error::HangupGrace(HANGUP_GRACE.as_secs()))))
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
