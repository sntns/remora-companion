use std::{io::Write, path::Path};

use error_stack::ResultExt;
use remora_channel::{
    adapter::ssh::{Error, Result, SshClientAdapter},
    model::{ExecInput, ExecOutcome, SshCommand, SshKeys},
};
use remora_progress::OperationContext;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Runs `ssh` as a child on this process's terminal.
///
/// A child rather than an exec, so the session's key material is removed
/// however ssh ends. Signals are relayed to ssh and never end this process
/// first: ssh decides what a signal means, and this process then cleans up
/// and exits with ssh's code. A terminal's Ctrl-C reaches ssh on its own
/// (and in an interactive session never becomes a signal at all, ssh having
/// put the terminal in raw mode), so relaying it too is harmless.
pub struct OpenSshClientImpl;

#[async_trait::async_trait]
impl SshClientAdapter for OpenSshClientImpl {
    async fn run(&self, command: &SshCommand, keys: &SshKeys) -> Result<i32> {
        // Removed when this drops, at the end of this call whichever way
        // it returns.
        let (_workdir, key_options) = write_keys(keys)?;
        // First, so ssh (first value wins) never lets a later option
        // replace the session's identity or its host pinning.
        let mut child = tokio::process::Command::new(&command.program)
            .args(&key_options)
            .args(&command.arguments)
            .kill_on_drop(false)
            .spawn()
            .change_context_lazy(|| Error::Spawn(command.program.clone()))?;

        let status = relay_until_exit(&mut child).await?;
        Ok(exit_code(status))
    }

    async fn exec(
        &self,
        command: &SshCommand,
        keys: &SshKeys,
        input: Option<&ExecInput>,
        echo: bool,
        ctx: &OperationContext,
    ) -> Result<ExecOutcome> {
        let (_workdir, key_options) = write_keys(keys)?;
        let mut child = tokio::process::Command::new(&command.program)
            .args(&key_options)
            .args(&command.arguments)
            .stdin(if input.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .change_context_lazy(|| Error::Spawn(command.program.clone()))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let feeding = async {
            match (input, stdin) {
                (Some(input), Some(stdin)) => feed(input, stdin, ctx).await,
                _ => Ok(0),
            }
        };
        let reading = async {
            tokio::join!(
                collect(stdout, echo.then_some(ctx)),
                collect(stderr, echo.then_some(ctx))
            )
        };
        let run = async {
            let (sent, (out, err)) = tokio::join!(feeding, reading);
            let status = child.wait().await.change_context(Error::Wait)?;
            Ok::<_, error_stack::Report<Error>>((sent, out, err, status))
        };
        let (sent, stdout, stderr, status): (Result<u64>, _, _, _) = tokio::select! {
            ran = run => ran?,
            // Dropping the run drops the child, which kill_on_drop kills.
            _ = ctx.cancel.cancelled() => return Err(error_stack::Report::new(Error::Cancelled)),
        };
        Ok(ExecOutcome {
            code: exit_code(status),
            stdout,
            stderr,
            // A command that ends without reading all of it (the connection
            // dropping) leaves the rest unsent: the caller tells from the
            // device what it got.
            sent: sent?,
        })
    }
}

/// Streams `input` from its offset to `stdin`, reporting the bytes sent
/// (counted from the file's start) out of its size; closes stdin at the
/// end so the remote command sees EOF. A write failing means the command
/// or the connection went away: what was sent until then is returned.
async fn feed(
    input: &ExecInput,
    mut stdin: tokio::process::ChildStdin,
    ctx: &OperationContext,
) -> Result<u64> {
    let read_error = || Error::Input(input.path.clone());
    let mut file = tokio::fs::File::open(&input.path)
        .await
        .change_context_lazy(read_error)?;
    let size = file.metadata().await.change_context_lazy(read_error)?.len();
    file.seek(std::io::SeekFrom::Start(input.offset))
        .await
        .change_context_lazy(read_error)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut sent = 0u64;
    ctx.sink.progress_bytes(input.offset, size);
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .change_context_lazy(read_error)?;
        if read == 0 {
            break;
        }
        if stdin.write_all(&buffer[..read]).await.is_err() {
            break;
        }
        sent += read as u64;
        ctx.sink.progress_bytes(input.offset + sent, size);
    }
    let _ = stdin.shutdown().await;
    Ok(sent)
}

/// Everything `stream` says, as text, one line per line it drew: split at
/// carriage returns too -- a progress bar redrawn in place is one line per
/// redraw -- and without terminal escapes (colours, cursor moves, line
/// clears), which a program drawing for a terminal sends regardless. Each
/// non-blank line is logged through `echo` as it comes.
async fn collect(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    echo: Option<&OperationContext>,
) -> String {
    let mut text = String::new();
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    let flush = |pending: &mut Vec<u8>, text: &mut String| {
        let line = plain(&String::from_utf8_lossy(pending));
        pending.clear();
        let line = line.trim_end();
        if line.trim().is_empty() {
            return;
        }
        if let Some(ctx) = echo {
            ctx.sink.log(line.trim().to_owned());
        }
        text.push_str(line);
        text.push('\n');
    };
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for &byte in &buffer[..read] {
            if byte == b'\n' || byte == b'\r' {
                flush(&mut pending, &mut text);
            } else {
                pending.push(byte);
            }
        }
    }
    flush(&mut pending, &mut text);
    text
}

/// `text` without its terminal escape sequences: CSI (`ESC [ … letter`),
/// OSC (`ESC ] … BEL`) and two-character ones.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                for c in chars.by_ref() {
                    if c == '\u{7}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Writes `keys` to a fresh private (0700) directory, each file 0600 --
/// it holds a private key, even if one that is worthless in a few minutes
/// -- and returns it with the options pointing ssh/scp at them.
fn write_keys(keys: &SshKeys) -> Result<(tempfile::TempDir, Vec<String>)> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("remora-ssh-");
    // Explicitly: tempfile only applies the umask, which is 0775 for many
    // users.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let workdir = builder.tempdir().change_context(Error::Keys)?;

    let identity = workdir.path().join("id_ed25519");
    write_private(&identity, keys.private_key().as_bytes())?;
    let certificate = workdir.path().join("id_ed25519-cert.pub");
    write_private(&certificate, line(&keys.certificate).as_bytes())?;
    let known_hosts = workdir.path().join("known_hosts");
    write_private(&known_hosts, line(&keys.known_hosts).as_bytes())?;

    let options = vec![
        "-i".to_owned(),
        identity.display().to_string(),
        "-o".to_owned(),
        format!("CertificateFile={}", certificate.display()),
        "-o".to_owned(),
        format!("UserKnownHostsFile={}", known_hosts.display()),
    ];
    Ok((workdir, options))
}

/// One newline-terminated text: ssh reads these files line by line.
fn line(text: &str) -> String {
    format!("{}\n", text.trim_end())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .change_context(Error::Keys)
        .attach_with(|| path.display().to_string())
}

#[cfg(unix)]
async fn relay_until_exit(child: &mut tokio::process::Child) -> Result<std::process::ExitStatus> {
    use tokio::signal::unix::{signal, SignalKind};

    let listen = |kind| signal(kind).change_context(Error::Wait);
    let mut interrupt = listen(SignalKind::interrupt())?;
    let mut terminate = listen(SignalKind::terminate())?;
    let mut hangup = listen(SignalKind::hangup())?;
    let mut quit = listen(SignalKind::quit())?;
    let pid = child.id();

    loop {
        let relayed = tokio::select! {
            status = child.wait() => return status.change_context(Error::Wait),
            _ = interrupt.recv() => libc::SIGINT,
            _ = terminate.recv() => libc::SIGTERM,
            _ = hangup.recv() => libc::SIGHUP,
            _ = quit.recv() => libc::SIGQUIT,
        };
        if let Some(pid) = pid {
            // SAFETY: kill(2) with a pid we spawned and still own (not yet
            // reaped, since wait() has not returned) and a valid signal.
            unsafe {
                libc::kill(pid as libc::pid_t, relayed);
            }
        }
    }
}

#[cfg(not(unix))]
async fn relay_until_exit(child: &mut tokio::process::Child) -> Result<std::process::ExitStatus> {
    // Ctrl-C reaches the child through the shared console; this process
    // only has to survive it to clean up afterwards.
    loop {
        tokio::select! {
            status = child.wait() => return status.change_context(Error::Wait),
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

/// ssh's own exit code, or what a shell reports for a signal (128 + n).
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    255
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn keys() -> SshKeys {
        SshKeys::new(
            "PRIVATE KEY\n".to_owned().into(),
            "CERT".into(),
            "@cert-authority * HOSTCA".into(),
        )
    }

    /// `script` as a client: sh, seeing the arguments the client would.
    struct Client {
        dir: tempfile::TempDir,
    }

    impl Client {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn command(&self, script: &str) -> SshCommand {
            let path = self.dir.path().join("client");
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            SshCommand {
                program: path,
                arguments: vec!["device".into()],
            }
        }
    }

    #[tokio::test]
    async fn returns_the_childs_exit_code() {
        let client = Client::new();
        for code in [0, 42] {
            let command = client.command(&format!("exit {code}"));
            assert_eq!(
                OpenSshClientImpl.run(&command, &keys()).await.unwrap(),
                code
            );
        }
    }

    #[tokio::test]
    async fn reports_a_signal_like_a_shell() {
        let client = Client::new();
        let command = client.command("kill -TERM $$");
        assert_eq!(
            OpenSshClientImpl.run(&command, &keys()).await.unwrap(),
            128 + 15
        );
    }

    #[tokio::test]
    async fn a_missing_client_is_a_spawn_error() {
        let command = SshCommand {
            program: "/nonexistent/ssh".into(),
            arguments: vec![],
        };
        let report = OpenSshClientImpl.run(&command, &keys()).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Spawn(_)));
    }

    #[tokio::test]
    async fn the_keys_exist_privately_only_while_the_client_runs() {
        let client = Client::new();
        let seen = client.dir.path().join("seen");
        // Records the files it was pointed at, and their modes, before the
        // client's own arguments.
        let command = client.command(&format!(
            "[ \"$1\" = -i ] || exit 1\n\
             dir=$(dirname \"$2\")\n\
             ls -ld \"$dir\" \"$2\" > {seen}\n\
             echo \"$dir\" >> {seen}\n\
             cat \"$2\" \"$dir/id_ed25519-cert.pub\" \"$dir/known_hosts\" >> {seen}\n\
             [ \"$4\" = \"CertificateFile=$dir/id_ed25519-cert.pub\" ] || exit 2\n\
             [ \"$6\" = \"UserKnownHostsFile=$dir/known_hosts\" ] || exit 3\n\
             [ \"$7\" = device ] || exit 4\n\
             exit 7",
            seen = seen.display()
        ));
        assert_eq!(OpenSshClientImpl.run(&command, &keys()).await.unwrap(), 7);

        let seen = std::fs::read_to_string(seen).unwrap();
        let lines: Vec<&str> = seen.lines().collect();
        assert!(lines[0].starts_with("drwx------"), "{seen}");
        assert!(lines[1].starts_with("-rw-------"), "{seen}");
        assert_eq!(
            &lines[3..],
            ["PRIVATE KEY", "CERT", "@cert-authority * HOSTCA"]
        );
        assert!(!std::path::Path::new(lines[2]).exists());
    }

    #[test]
    fn terminal_escapes_are_stripped() {
        assert_eq!(
            plain("\u{1b}[2K\u{1b}[36m[████░░] 45%\u{1b}[0m Copying"),
            "[████░░] 45% Copying"
        );
        assert_eq!(plain("\u{1b}]0;title\u{7}done"), "done");
    }
}
