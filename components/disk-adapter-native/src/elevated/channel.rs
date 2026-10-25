//! The helper's channel: what the unprivileged program listens on and the
//! elevated helper connects to.

use std::io;

#[cfg(unix)]
pub use unix::{connect, Listener, Stream};
#[cfg(windows)]
pub use windows::{connect, Listener, Stream};

/// How long the listener waits between looks at a connection and at the
/// helper having exited (refused, failed).
const POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// The helper exited before it connected: the rights not given (a wrong
/// password, a cancelled dialog), or it failed.
fn exited(code: i32) -> io::Error {
    io::Error::other(format!("the helper exited before connecting (code {code})"))
}

#[cfg(unix)]
mod unix {
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::*;

    pub type Stream = UnixStream;

    /// A socket in a directory of its own that only this user can reach
    /// (0700) -- and an administrator, who is who connects.
    pub struct Listener {
        listener: UnixListener,
        name: String,
        _dir: tempfile::TempDir,
    }

    impl Listener {
        pub fn create() -> io::Result<Self> {
            let dir = tempfile::Builder::new().prefix("remora-disk-").tempdir()?;
            let path = dir.path().join("helper");
            let listener = UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            Ok(Self {
                listener,
                name: path.to_string_lossy().into_owned(),
                _dir: dir,
            })
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        /// The helper's connection, once it made it: as long as the user
        /// takes to give the rights. Fails if the helper exits first.
        pub fn accept(self, process: &mut remora_elevate::Elevated) -> io::Result<Stream> {
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false)?;
                        return Ok(stream);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
                if let Some(code) = process.try_wait()? {
                    return Err(exited(code));
                }
                std::thread::sleep(POLL);
            }
        }
    }

    pub fn connect(name: &str) -> io::Result<Stream> {
        UnixStream::connect(name)
    }
}

#[cfg(windows)]
mod windows {
    use std::{
        fs::File,
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        sync::mpsc,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        },
    };

    use super::*;

    pub type Stream = File;

    /// A named pipe, one instance, local clients only; and of those, only
    /// the helper's own process is kept.
    pub struct Listener {
        pipe: isize,
        name: String,
    }

    impl Listener {
        pub fn create() -> io::Result<Self> {
            let name = format!(r"\\.\pipe\remora-disk-{}", random_hex());
            let wide: Vec<u16> = std::ffi::OsStr::new(&name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: a NUL-terminated name; default security (the
            // creator, administrators and the system have full access).
            let pipe = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    64 * 1024,
                    64 * 1024,
                    0,
                    std::ptr::null(),
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                pipe: pipe as isize,
                name,
            })
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        /// The helper's connection: waits for it on a thread, while this
        /// one watches the helper exit first (refused, failed), in which
        /// case it connects itself to let the waiting thread go.
        pub fn accept(self, process: &mut remora_elevate::Elevated) -> io::Result<Stream> {
            let pipe = self.pipe;
            let (connected, outcome) = mpsc::channel();
            std::thread::spawn(move || {
                // SAFETY: the pipe created above, still open.
                let ok = unsafe { ConnectNamedPipe(pipe as _, std::ptr::null_mut()) } != 0;
                // SAFETY: no arguments.
                let ok = ok || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                let _ = connected.send(ok);
            });
            let exited_first = loop {
                match outcome.recv_timeout(POLL) {
                    Ok(true) => break None,
                    Ok(false) => {
                        let error = io::Error::last_os_error();
                        self.close();
                        return Err(error);
                    }
                    Err(_) => {}
                }
                if let Some(code) = process.try_wait()? {
                    break Some(code);
                }
            };
            if let Some(code) = exited_first {
                let _ = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.name);
                let _ = outcome.recv();
                self.close();
                return Err(exited(code));
            }
            let mut client = 0u32;
            // SAFETY: a connected pipe, an out pointer.
            unsafe { GetNamedPipeClientProcessId(pipe as _, &mut client) };
            if Some(client) != process.id() {
                self.close();
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "another process connected to the helper's pipe",
                ));
            }
            let pipe = std::mem::ManuallyDrop::new(self).pipe;
            // SAFETY: the pipe handle, owned from now on by the File.
            Ok(unsafe { File::from_raw_handle(pipe as _) })
        }

        fn close(self) {
            drop(self);
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            // SAFETY: the pipe created above, closed once.
            unsafe { CloseHandle(self.pipe as _) };
        }
    }

    pub fn connect(name: &str) -> io::Result<Stream> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
    }

    fn random_hex() -> String {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.write_u32(std::process::id());
        format!("{:016x}", hasher.finish())
    }
}
