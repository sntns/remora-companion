use error_stack::ResultExt;
use remora_channel::{
    adapter::ssh::{Error, Result, SshClientAdapter},
    model::SshCommand,
};

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
    async fn run(&self, command: &SshCommand) -> Result<i32> {
        let mut child = tokio::process::Command::new(&command.program)
            .args(&command.arguments)
            .kill_on_drop(false)
            .spawn()
            .change_context_lazy(|| Error::Spawn(command.program.clone()))?;

        let status = relay_until_exit(&mut child).await?;
        Ok(exit_code(status))
    }
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
    use super::*;

    fn sh(script: &str) -> SshCommand {
        SshCommand {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), script.into()],
        }
    }

    #[tokio::test]
    async fn returns_the_childs_exit_code() {
        assert_eq!(OpenSshClientImpl.run(&sh("exit 0")).await.unwrap(), 0);
        assert_eq!(OpenSshClientImpl.run(&sh("exit 42")).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn reports_a_signal_like_a_shell() {
        assert_eq!(
            OpenSshClientImpl.run(&sh("kill -TERM $$")).await.unwrap(),
            128 + 15
        );
    }

    #[tokio::test]
    async fn a_missing_client_is_a_spawn_error() {
        let command = SshCommand {
            program: "/nonexistent/ssh".into(),
            arguments: vec![],
        };
        let report = OpenSshClientImpl.run(&command).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Spawn(_)));
    }
}
