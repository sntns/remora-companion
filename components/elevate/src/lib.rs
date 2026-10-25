//! Starting a process with administrator rights, asked for the way the OS
//! asks: `sudo` in a terminal, `pkexec` on a Linux desktop, the
//! administrator dialog (`osascript`) on macOS, UAC on Windows. What runs
//! elevated is kept to what needs it -- a disk opened and written, by the
//! disk adapter's helper -- the rest of the program never is.
//!
//! The elevated process gets no stdio of ours (UAC can't hand it any, and
//! `sudo` may put it in a pty of its own): it talks over a channel its
//! arguments name. [`spawn`] starts it and hands back a handle to see it
//! exit, so that a refusal (a wrong password, a cancelled dialog) is told
//! from a process that is merely slow to connect.
//!
//! A utility crate, not a vertical: no business decision, only the process
//! and the platform's tools. Not a port either -- elevating is the shell's
//! affair, never behavior a test substitutes (same reasoning as
//! `remora-tui`).

use std::{ffi::OsString, io, path::Path};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no way to ask for administrator rights here (no terminal for sudo, no desktop dialog): run it as an administrator")]
    NoTool,
    #[error("the administrator rights were not given")]
    Refused,
    #[error("failed to start a process with administrator rights")]
    Spawn(#[source] io::Error),
}

/// Whether this process already has administrator rights: root, or on
/// Windows an elevated token.
pub fn is_elevated() -> bool {
    platform::is_elevated()
}

/// Starts `program` with `args` and administrator rights, asking for them
/// with the platform's tool; `reason` says what needs them, in its prompt.
/// Returns at once: the process runs on, see [`Elevated`].
pub fn spawn(program: &Path, args: &[OsString], reason: &str) -> Result<Elevated, Error> {
    platform::spawn(program, args, reason)
}

/// An elevated process [`spawn`] started.
pub struct Elevated(platform::Process);

impl Elevated {
    /// Its exit code once it has exited; `None` while it runs (or waits
    /// for the user to give the rights).
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.0.try_wait()
    }

    /// Its process id, where the platform gives the elevated process's own
    /// (Windows); `None` where a tool stands between (sudo, pkexec).
    pub fn id(&self) -> Option<u32> {
        self.0.id()
    }

    /// Waits for it to exit.
    pub fn wait(mut self) -> io::Result<i32> {
        loop {
            if let Some(code) = self.try_wait()? {
                return Ok(code);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

impl std::fmt::Debug for Elevated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Elevated")
    }
}

#[cfg(unix)]
mod platform {
    use std::{
        io::IsTerminal,
        path::PathBuf,
        process::{Child, Command, Stdio},
    };

    use super::*;

    pub fn is_elevated() -> bool {
        // SAFETY: no arguments, no failure.
        unsafe { libc::geteuid() == 0 }
    }

    pub struct Process(Child);

    impl Process {
        pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
            Ok(self.0.try_wait()?.map(|status| status.code().unwrap_or(1)))
        }

        pub fn id(&self) -> Option<u32> {
            None
        }
    }

    /// A program on the `PATH`.
    fn on_path(program: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
    }

    /// The elevated process keeps none of our stdio: it talks over its
    /// channel, and our terminal stays ours (the progress drawn on it).
    fn quiet(command: &mut Command) -> &mut Command {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
    }

    pub fn spawn(program: &Path, args: &[OsString], reason: &str) -> Result<Elevated, Error> {
        let terminal = io::stdin().is_terminal() && io::stderr().is_terminal();
        if let Some(sudo) = on_path("sudo") {
            // sudo asks on the terminal itself (/dev/tty), whatever the
            // command's stdio; without one, only a password-less rule may.
            let mut command = Command::new(sudo);
            if terminal {
                command
                    .arg("-p")
                    .arg(format!("{reason}. Password for %p: "));
            } else {
                command.arg("-n");
            }
            if terminal || password_less(&command) {
                command.arg("--").arg(program).args(args);
                return start(quiet(&mut command));
            }
        }
        graphical(program, args, reason)
    }

    /// Whether `sudo -n` would run without asking (a password-less rule, or
    /// credentials still cached).
    fn password_less(sudo: &Command) -> bool {
        Command::new(sudo.get_program())
            .args(["-n", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn start(command: &mut Command) -> Result<Elevated, Error> {
        let child = command.spawn().map_err(Error::Spawn)?;
        Ok(Elevated(Process(child)))
    }

    #[cfg(target_os = "linux")]
    fn graphical(program: &Path, args: &[OsString], _reason: &str) -> Result<Elevated, Error> {
        let desktop =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        match on_path("pkexec") {
            // polkit's own dialog.
            Some(pkexec) if desktop => start(quiet(Command::new(pkexec).arg(program).args(args))),
            _ => Err(Error::NoTool),
        }
    }

    #[cfg(target_os = "macos")]
    fn graphical(program: &Path, args: &[OsString], reason: &str) -> Result<Elevated, Error> {
        // The administrator dialog, through AppleScript; the command runs
        // as long as osascript does.
        let mut line = shell_quote(program.as_os_str());
        for arg in args {
            line.push(' ');
            line.push_str(&shell_quote(arg));
        }
        let script = format!(
            "do shell script {} with prompt {} with administrator privileges",
            applescript_string(&line),
            applescript_string(reason),
        );
        start(quiet(
            Command::new("/usr/bin/osascript").arg("-e").arg(script),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn graphical(_program: &Path, _args: &[OsString], _reason: &str) -> Result<Elevated, Error> {
        Err(Error::NoTool)
    }

    /// A word for `sh`, quoted whatever it holds.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn shell_quote(word: &std::ffi::OsStr) -> String {
        format!("'{}'", word.to_string_lossy().replace('\'', r"'\''"))
    }

    /// An AppleScript string literal.
    #[cfg(target_os = "macos")]
    fn applescript_string(text: &str) -> String {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0},
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::Threading::{
            GetCurrentProcess, GetExitCodeProcess, GetProcessId, OpenProcessToken,
            WaitForSingleObject,
        },
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
            WindowsAndMessaging::SW_HIDE,
        },
    };

    use super::*;

    pub fn is_elevated() -> bool {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the current process's pseudo handle, and an out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        // SAFETY: a token handle just opened, a buffer of the size given.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
        };
        // SAFETY: the handle opened above.
        unsafe { CloseHandle(token) };
        ok != 0 && elevation.TokenIsElevated != 0
    }

    /// An elevated process's handle.
    pub struct Process(HANDLE);

    // SAFETY: a process handle may be used from any thread.
    unsafe impl Send for Process {}

    impl Process {
        pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
            // SAFETY: the handle ShellExecuteExW gave us, still open.
            if unsafe { WaitForSingleObject(self.0, 0) } != WAIT_OBJECT_0 {
                return Ok(None);
            }
            let mut code = 1u32;
            // SAFETY: as above.
            unsafe { GetExitCodeProcess(self.0, &mut code) };
            Ok(Some(code as i32))
        }

        pub fn id(&self) -> Option<u32> {
            // SAFETY: the handle ShellExecuteExW gave us, still open.
            Some(unsafe { GetProcessId(self.0) })
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: the handle ShellExecuteExW gave us, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// UAC's prompt, then the process without a window: it talks over its
    /// channel. Started in this directory, so relative paths still hold.
    pub fn spawn(program: &Path, args: &[OsString], _reason: &str) -> Result<Elevated, Error> {
        let wide = |text: &std::ffi::OsStr| -> Vec<u16> {
            text.encode_wide().chain(std::iter::once(0)).collect()
        };
        let parameters: Vec<String> = args
            .iter()
            .map(|arg| quote_argument(&arg.to_string_lossy()))
            .collect();
        let verb = wide("runas".as_ref());
        let file = wide(program.as_os_str());
        let parameters = wide(parameters.join(" ").as_ref());
        let directory = std::env::current_dir()
            .map(|dir| wide(dir.as_os_str()))
            .unwrap_or_else(|_| vec![0]);
        // SAFETY: zeroed is a valid SHELLEXECUTEINFOW, its fields set below.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.lpDirectory = directory.as_ptr();
        info.nShow = SW_HIDE;
        // SAFETY: every pointer outlives the call.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            // SAFETY: no arguments.
            let error = unsafe { GetLastError() };
            if error == ERROR_CANCELLED {
                return Err(Error::Refused);
            }
            return Err(Error::Spawn(io::Error::from_raw_os_error(error as i32)));
        }
        Ok(Elevated(Process(info.hProcess)))
    }

    /// An argument as the C runtime's command-line parsing reads it back.
    pub(super) fn quote_argument(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_owned();
        }
        let mut quoted = String::from('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.push_str(&"\\".repeat(backslashes));
                    quoted.push(c);
                    backslashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(backslashes * 2));
        quoted.push('"');
        quoted
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn arguments_round_trip_through_the_command_line() {
            assert_eq!(quote_argument("disk-helper"), "disk-helper");
            assert_eq!(
                quote_argument(r"\\.\pipe\remora disk"),
                r#""\\.\pipe\remora disk""#
            );
            assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
            assert_eq!(quote_argument(r"dir\ "), r#""dir\ ""#);
            assert_eq!(quote_argument(r"end\"), r"end\");
            assert_eq!(quote_argument(""), r#""""#);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn words_are_quoted_for_sh() {
        use std::ffi::OsStr;
        assert_eq!(
            super::platform::shell_quote(OsStr::new("disk-helper")),
            "'disk-helper'"
        );
        assert_eq!(
            super::platform::shell_quote(OsStr::new("it's")),
            r"'it'\''s'"
        );
    }
}
