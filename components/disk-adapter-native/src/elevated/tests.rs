use std::{os::unix::net::UnixStream, path::PathBuf, thread};

use super::*;

/// A helper on one end of a socket pair, opening a temporary file in place
/// of the disk; the program's side on the other.
fn helper(
    open: impl FnOnce(&Path) -> Result<DiskTarget> + Send + 'static,
) -> (RemoteDisk, thread::JoinHandle<io::Result<()>>) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let served = thread::spawn(move || serve_on(theirs, Path::new("/dev/test"), open));
    (
        RemoteDisk {
            stream: ours,
            _process: None,
        },
        served,
    )
}

fn file_disk(path: PathBuf) -> impl FnOnce(&Path) -> Result<DiskTarget> {
    move |_| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map(DiskTarget::new)
            .map_err(|e| Report::new(e).change_context(Error::Open(path.clone())))
    }
}

#[test]
fn writes_land_on_the_disk_the_helper_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    std::fs::write(&path, vec![0u8; 64]).unwrap();
    let (mut disk, served) = helper(file_disk(path.clone()));

    disk.call(OPEN, &[]).unwrap();
    disk.write_all(b"head").unwrap();
    assert_eq!(disk.seek(SeekFrom::Start(32)).unwrap(), 32);
    disk.write_all(b"middle").unwrap();
    assert_eq!(disk.seek(SeekFrom::Current(-6)).unwrap(), 32);
    assert_eq!(disk.seek(SeekFrom::End(-1)).unwrap(), 63);
    disk.flush().unwrap();
    disk.sync().unwrap();
    // A big write goes in several requests, and arrives whole.
    disk.seek(SeekFrom::Start(0)).unwrap();
    let big = vec![7u8; MAX_WRITE + 10];
    disk.write_all(&big).unwrap();
    assert!(!Box::new(disk).eject().unwrap());
    served.join().unwrap().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), big.len());
    assert!(written.iter().all(|byte| *byte == 7));
}

#[test]
fn the_helpers_refusal_reaches_the_program() {
    let (mut disk, served) = helper(|path| {
        Err(Report::new(Error::SystemDisk(path.to_path_buf())).attach("seen by the helper"))
    });
    let error = disk.call(OPEN, &[]).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("refusing to unmount /dev/test"),
        "{message}"
    );
    assert!(message.contains("seen by the helper"), "{message}");
    drop(disk);
    served.join().unwrap().unwrap();
}

#[test]
fn nothing_is_written_before_the_disk_is_open_and_a_hangup_ends_the_helper() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    std::fs::write(&path, b"").unwrap();
    let (mut disk, served) = helper(file_disk(path.clone()));
    assert!(disk.write(b"early").is_err());
    // Why the helper failed to open it survives the trip.
    let (mut missing, served_missing) = helper(file_disk(dir.path().join("missing").join("disk")));
    let error = missing.call(OPEN, &[]).unwrap_err();
    assert!(error.to_string().contains("No such file"), "{error}");
    drop(missing);
    served_missing.join().unwrap().unwrap();

    drop(disk);
    served.join().unwrap().unwrap();
    assert!(std::fs::read(&path).unwrap().is_empty());
}

#[test]
fn seeks_survive_the_trip() {
    for pos in [
        SeekFrom::Start(u64::MAX >> 1),
        SeekFrom::End(-512),
        SeekFrom::Current(4096),
    ] {
        assert_eq!(decode_seek(&encode_seek(pos)).unwrap(), pos);
    }
    assert!(decode_seek(&[9; 9]).is_err());
}
