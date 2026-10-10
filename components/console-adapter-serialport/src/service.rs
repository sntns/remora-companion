use std::time::Duration;

use error_stack::{Report, ResultExt};
use remora_console::{
    adapter::serial::{Error, Result, SerialLink, SerialPortAdapter, SerialReader, SerialWriter},
    model::SerialSettings,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPort, SerialPortBuilderExt, StopBits};

/// How long a break holds the line: well over the 250 ms ttys take a break
/// to be, as picocom and minicom send.
const BREAK: Duration = Duration::from_millis(500);

pub struct SerialPortAdapterImpl;

#[async_trait::async_trait]
impl SerialPortAdapter for SerialPortAdapterImpl {
    async fn open(&self, settings: &SerialSettings) -> Result<SerialLink> {
        #[allow(unused_mut)]
        let mut port = tokio_serial::new(&settings.port, settings.baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .open_native_async()
            .change_context_lazy(|| Error::Open(settings.port.clone()))?;
        // Two programs on one console garble each other: refuse to share it,
        // as picocom's lock does.
        #[cfg(unix)]
        port.set_exclusive(true)
            .change_context_lazy(|| Error::Open(settings.port.clone()))?;

        let (chunks, received) = mpsc::channel(64);
        let (commands, pending) = mpsc::channel(64);
        tokio::spawn(serve(port, chunks, pending));
        Ok(SerialLink {
            reader: Box::new(Reader(received)),
            writer: Box::new(Writer(commands)),
        })
    }
}

enum Command {
    Write(Vec<u8>, oneshot::Sender<std::io::Result<()>>),
    Break(oneshot::Sender<std::io::Result<()>>),
}

/// Owns the port until the link is dropped or the port fails.
async fn serve(
    mut port: tokio_serial::SerialStream,
    chunks: mpsc::Sender<std::io::Result<Vec<u8>>>,
    mut pending: mpsc::Receiver<Command>,
) {
    let mut buffer = vec![0; 4096];
    loop {
        tokio::select! {
            read = port.read(&mut buffer) => match read {
                // A tty at end of file is a port that went away.
                Ok(0) => return,
                Ok(n) => {
                    if chunks.send(Ok(buffer[..n].to_vec())).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = chunks.send(Err(error)).await;
                    return;
                }
            },
            command = pending.recv() => match command {
                None => return,
                Some(Command::Write(bytes, done)) => {
                    let written = async {
                        port.write_all(&bytes).await?;
                        port.flush().await
                    }
                    .await;
                    let _ = done.send(written);
                }
                Some(Command::Break(done)) => {
                    let sent = async {
                        port.set_break().map_err(std::io::Error::from)?;
                        tokio::time::sleep(BREAK).await;
                        port.clear_break().map_err(std::io::Error::from)
                    }
                    .await;
                    let _ = done.send(sent);
                }
            },
        }
    }
}

struct Reader(mpsc::Receiver<std::io::Result<Vec<u8>>>);

#[async_trait::async_trait]
impl SerialReader for Reader {
    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        match self.0.recv().await {
            None => Ok(None),
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(error)) => Err(Report::new(error).change_context(Error::Io)),
        }
    }
}

struct Writer(mpsc::Sender<Command>);

impl Writer {
    async fn run(
        &self,
        command: impl FnOnce(oneshot::Sender<std::io::Result<()>>) -> Command,
    ) -> Result<()> {
        let (done, outcome) = oneshot::channel();
        self.0
            .send(command(done))
            .await
            .map_err(|_| Report::new(Error::Closed))?;
        outcome
            .await
            .map_err(|_| Report::new(Error::Closed))?
            .map_err(|error| Report::new(error).change_context(Error::Io))
    }
}

#[async_trait::async_trait]
impl SerialWriter for Writer {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let bytes = bytes.to_vec();
        self.run(|done| Command::Write(bytes, done)).await
    }

    async fn send_break(&mut self) -> Result<()> {
        self.run(Command::Break).await
    }
}

// A pseudo-terminal stands in for the device's end of the cable.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn carries_bytes_both_ways_and_refuses_a_second_opener() {
        let pty = nix::pty::openpty(None, None).unwrap();
        let path = nix::unistd::ttyname(&pty.slave).unwrap();
        // Raw, so the line discipline passes bytes as they are.
        let mut attributes = nix::sys::termios::tcgetattr(&pty.slave).unwrap();
        nix::sys::termios::cfmakeraw(&mut attributes);
        nix::sys::termios::tcsetattr(&pty.slave, nix::sys::termios::SetArg::TCSANOW, &attributes)
            .unwrap();

        let settings = SerialSettings {
            port: path.display().to_string(),
            baud: 115_200,
        };
        let mut link = SerialPortAdapterImpl.open(&settings).await.unwrap();

        // The device speaks...
        nix::unistd::write(&pty.master, b"login: ").unwrap();
        let mut seen = Vec::new();
        while seen.len() < 7 {
            seen.extend(link.reader.read().await.unwrap().unwrap());
        }
        assert_eq!(seen, b"login: ");

        // ...and hears what's typed.
        link.writer.write(b"root\r").await.unwrap();
        let mut heard = [0u8; 5];
        let mut got = 0;
        while got < 5 {
            got += nix::unistd::read(&pty.master, &mut heard[got..]).unwrap();
        }
        assert_eq!(&heard, b"root\r");

        assert!(SerialPortAdapterImpl.open(&settings).await.is_err());
    }
}
