//! A disk opened and written by an elevated helper, and nothing else
//! elevated: when the OS lets only an administrator write a disk, the
//! program starts itself again with administrator rights -- asked for with
//! the platform's tool (`remora-elevate`) -- to run just the helper, which
//! opens that one disk the usual way (volumes unmounted or locked, the
//! system disk refused) and then does what the unprivileged program asks of
//! it: write, seek, flush, sync, eject. Reading the image, downloading it,
//! drawing the progress all stay unprivileged.
//!
//! The helper talks over a channel its arguments name (a Unix socket in a
//! directory only the user can reach; a named pipe on Windows, only its
//! own process accepted), never over stdio: UAC hands it none, and `sudo`
//! may put it in a pty of its own. It opens only the disk its own command
//! line names -- the one the administrator prompt was about.
//!
//! The protocol, little-endian: a request is an op byte, a `u32` payload
//! length and the payload; an answer is a status byte (0 ok, 1 error) and
//! a `u64` value, and for an error, the OS error code (`i32`, -1 for
//! none), an [`io::ErrorKind`] byte and a `u32`-long message.

mod channel;

use std::{
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use error_stack::{Report, ResultExt};
use remora_disk::{
    adapter::{DiskTarget, DiskWrite, Error, Result},
    model::DiskInfo,
};

pub use channel::Stream;

const OPEN: u8 = 1;
const WRITE: u8 = 2;
const SEEK: u8 = 3;
const FLUSH: u8 = 4;
const SYNC: u8 = 5;
const EJECT: u8 = 6;

/// The most one write request carries: a bigger write goes in several.
const MAX_WRITE: usize = 4 * 1024 * 1024;

/// How to start the helper: this program, with these arguments before the
/// channel and the disk (`--channel <name> --disk <path>` are added).
#[derive(Debug, Clone)]
pub struct Helper {
    pub args: Vec<OsString>,
}

impl Helper {
    /// The disk `disk` describes, opened by an elevated helper: asks for
    /// the administrator rights (the platform's prompt, now), then has the
    /// helper open it.
    pub fn open(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        let path = &disk.path;
        let denied = || Error::PermissionDenied(path.clone());
        let program = std::env::current_exe().change_context_lazy(denied)?;
        let channel = channel::Listener::create().change_context_lazy(denied)?;
        let mut args = self.args.clone();
        args.extend([
            "--channel".into(),
            channel.name().into(),
            "--disk".into(),
            path.clone().into_os_string(),
        ]);
        let reason = format!("Writing {} takes administrator rights", path.display());
        let mut process = remora_elevate::spawn(&program, &args, &reason)
            .map_err(|error| Report::new(error).change_context(denied()))?;
        let stream = channel
            .accept(&mut process)
            .change_context_lazy(denied)
            .attach("the administrator rights were not given, or the helper failed")?;
        let mut disk = RemoteDisk {
            stream,
            _process: Some(process),
        };
        disk.call(OPEN, &[])
            .change_context_lazy(|| Error::Open(path.clone()))?;
        Ok(DiskTarget::new(disk))
    }
}

/// The helper's side: connects to `channel`, opens the disk at `disk` with
/// `open` when asked to, and does what it is asked until the program hangs
/// up or ejects the disk.
pub fn serve(
    channel: &str,
    disk: &Path,
    open: impl FnOnce(&Path) -> Result<DiskTarget>,
) -> io::Result<()> {
    let stream = channel::connect(channel)?;
    serve_on(stream, disk, open)
}

fn serve_on(
    mut stream: impl Read + Write,
    disk: &Path,
    open: impl FnOnce(&Path) -> Result<DiskTarget>,
) -> io::Result<()> {
    let mut open = Some(open);
    let mut target: Option<DiskTarget> = None;
    loop {
        let Some((op, payload)) = read_request(&mut stream)? else {
            // Hung up: the target closes (and its volumes are let go).
            return Ok(());
        };
        let answer: io::Result<u64> = match (op, target.as_mut()) {
            (OPEN, None) => match open.take().map(|open| open(disk)) {
                Some(Ok(opened)) => {
                    target = Some(opened);
                    Ok(0)
                }
                Some(Err(report)) => Err(io::Error::other(one_line(&report))),
                None => Err(io::Error::other("the disk was already opened")),
            },
            (WRITE, Some(target)) => target.write_all(&payload).map(|()| payload.len() as u64),
            (SEEK, Some(target)) => decode_seek(&payload).and_then(|pos| target.seek(pos)),
            (FLUSH, Some(target)) => target.flush().map(|()| 0),
            (SYNC, Some(target)) => target.sync().map(|()| 0),
            (EJECT, Some(_)) => {
                let ejected = target.take().expect("matched Some").eject();
                write_answer(&mut stream, &ejected.map(u64::from))?;
                return Ok(());
            }
            _ => Err(io::Error::other(format!("unexpected request {op}"))),
        };
        write_answer(&mut stream, &answer)?;
    }
}

/// A report on one line: its contexts and printable attachments, outermost
/// first, as the program shows the helper's refusal.
fn one_line(report: &Report<Error>) -> String {
    use error_stack::{AttachmentKind, FrameKind};
    let mut parts: Vec<String> = Vec::new();
    for frame in report.frames() {
        let part = match frame.kind() {
            FrameKind::Context(context) => context.to_string(),
            FrameKind::Attachment(AttachmentKind::Printable(printable)) => printable.to_string(),
            _ => continue,
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts.join(": ")
}

/// The disk an elevated helper holds open, written through its channel.
struct RemoteDisk {
    stream: Stream,
    /// The helper; it exits once the channel closes.
    _process: Option<remora_elevate::Elevated>,
}

impl RemoteDisk {
    fn call(&mut self, op: u8, payload: &[u8]) -> io::Result<u64> {
        write_request(&mut self.stream, op, payload)?;
        read_answer(&mut self.stream)
    }
}

impl Write for RemoteDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk = &buf[..buf.len().min(MAX_WRITE)];
        self.call(WRITE, chunk).map(|written| written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.call(FLUSH, &[]).map(|_| ())
    }
}

impl Seek for RemoteDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.call(SEEK, &encode_seek(pos))
    }
}

impl DiskWrite for RemoteDisk {
    fn sync(&mut self) -> io::Result<()> {
        self.call(SYNC, &[]).map(|_| ())
    }

    fn eject(mut self: Box<Self>) -> io::Result<bool> {
        self.call(EJECT, &[]).map(|ejected| ejected != 0)
    }
}

fn encode_seek(pos: SeekFrom) -> [u8; 9] {
    let (whence, offset) = match pos {
        SeekFrom::Start(offset) => (0u8, offset as i64),
        SeekFrom::End(offset) => (1, offset),
        SeekFrom::Current(offset) => (2, offset),
    };
    let mut bytes = [0u8; 9];
    bytes[0] = whence;
    bytes[1..].copy_from_slice(&offset.to_le_bytes());
    bytes
}

fn decode_seek(bytes: &[u8]) -> io::Result<SeekFrom> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "a malformed seek");
    let offset = i64::from_le_bytes(bytes.get(1..9).ok_or_else(invalid)?.try_into().unwrap());
    match bytes.first() {
        Some(0) => Ok(SeekFrom::Start(offset as u64)),
        Some(1) => Ok(SeekFrom::End(offset)),
        Some(2) => Ok(SeekFrom::Current(offset)),
        _ => Err(invalid()),
    }
}

fn write_request(stream: &mut impl Write, op: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 5];
    header[0] = op;
    header[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    stream.write_all(&header)?;
    stream.write_all(payload)?;
    stream.flush()
}

/// The next request; `None` once the program hung up.
fn read_request(stream: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 5];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    if length > MAX_WRITE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a request too long",
        ));
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(Some((header[0], payload)))
}

fn write_answer(stream: &mut impl Write, answer: &io::Result<u64>) -> io::Result<()> {
    let mut bytes = Vec::with_capacity(9);
    match answer {
        Ok(value) => {
            bytes.push(0);
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Err(error) => {
            bytes.push(1);
            bytes.extend_from_slice(&0u64.to_le_bytes());
            bytes.extend_from_slice(&error.raw_os_error().unwrap_or(-1).to_le_bytes());
            bytes.push(kind_code(error.kind()));
            let message = error.to_string();
            bytes.extend_from_slice(&(message.len() as u32).to_le_bytes());
            bytes.extend_from_slice(message.as_bytes());
        }
    }
    stream.write_all(&bytes)?;
    stream.flush()
}

fn read_answer(stream: &mut impl Read) -> io::Result<u64> {
    let mut head = [0u8; 9];
    stream.read_exact(&mut head)?;
    let value = u64::from_le_bytes(head[1..].try_into().unwrap());
    if head[0] == 0 {
        return Ok(value);
    }
    let mut error = [0u8; 9];
    stream.read_exact(&mut error)?;
    let code = i32::from_le_bytes(error[..4].try_into().unwrap());
    let kind = kind_of(error[4]);
    let length = u32::from_le_bytes(error[5..].try_into().unwrap()) as usize;
    let mut message = vec![0u8; length.min(64 * 1024)];
    stream.read_exact(&mut message)?;
    let message = String::from_utf8_lossy(&message).into_owned();
    Err(if code >= 0 {
        // The OS's own words for its code, as the helper got it.
        io::Error::from_raw_os_error(code)
    } else {
        io::Error::new(kind, message)
    })
}

/// The kinds worth telling apart across the channel; anything else is
/// `Other`, its message kept.
const KINDS: [io::ErrorKind; 6] = [
    io::ErrorKind::Other,
    io::ErrorKind::NotFound,
    io::ErrorKind::PermissionDenied,
    io::ErrorKind::InvalidInput,
    io::ErrorKind::InvalidData,
    io::ErrorKind::UnexpectedEof,
];

fn kind_code(kind: io::ErrorKind) -> u8 {
    KINDS.iter().position(|k| *k == kind).unwrap_or(0) as u8
}

fn kind_of(code: u8) -> io::ErrorKind {
    KINDS
        .get(usize::from(code))
        .copied()
        .unwrap_or(io::ErrorKind::Other)
}

#[cfg(all(test, unix))]
mod tests;
