//! What DiskArbitration has to agree to before a disk is overwritten: its
//! volumes unmounted (macOS refuses to open a disk with a mounted volume
//! for writing), and none mounted again while the copy rewrites the
//! partition table under them.

use std::{
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    disk::whole_disk_names,
    ffi::{self, Cf, DADiskRef, DADissenterRef, IoObject, Session},
};

/// How long DiskArbitration gets to answer: a busy volume refuses at once,
/// a slow disk flushing a big write may take a while to unmount.
const ARBITRATION_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the mount blocker's thread checks it has been dropped.
const BLOCKER_POLL: f64 = 0.1;

#[derive(Debug, thiserror::Error)]
pub enum ArbitrationError {
    #[error("DiskArbitration is not available")]
    Session,
    #[error("DiskArbitration doesn't know {0}")]
    NoDisk(String),
    #[error("{disk} wouldn't {verb}: {reason}")]
    Refused {
        disk: String,
        verb: &'static str,
        reason: String,
    },
    #[error("{disk} didn't {verb} in time")]
    TimedOut { disk: String, verb: &'static str },
}

type Result<T> = std::result::Result<T, ArbitrationError>;

/// Unmounts every volume of the whole disk `target`, including those of an
/// APFS container on one of its partitions (a disk of its own to
/// DiskArbitration: unmounting `target` alone would leave them mounted).
pub fn unmount(target: &str) -> Result<()> {
    let session = Session::new().ok_or(ArbitrationError::Session)?;
    let mut disks: Vec<String> = whole_disk_names()
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name != target)
        .filter(|name| {
            session.disk(name).is_some_and(|disk| {
                let lineage = disk.lineage();
                lineage.synthesized && lineage.whole_disks.contains(target)
            })
        })
        .collect();
    disks.push(target.to_owned());
    scheduled(&session, || {
        disks
            .iter()
            .try_for_each(|name| arbitrate(&session, name, Request::Unmount))
    })
}

/// Ejects the whole disk `target`, its volumes already unmounted: the
/// drive lets the media go (a card reader's light goes off), and the disk
/// and every disk carved from it leave `/dev`.
pub fn eject(target: &str) -> Result<()> {
    let session = Session::new().ok_or(ArbitrationError::Session)?;
    scheduled(&session, || arbitrate(&session, target, Request::Eject))
}

/// Runs `f` with `session` scheduled on this thread's run loop, where its
/// requests' callbacks are called.
fn scheduled<T>(session: &Session, f: impl FnOnce() -> T) -> T {
    // SAFETY: a live session, unscheduled before it's released.
    unsafe {
        let run_loop = ffi::CFRunLoopGetCurrent();
        ffi::DASessionScheduleWithRunLoop(session.as_ptr(), run_loop, ffi::kCFRunLoopDefaultMode);
        let result = f();
        ffi::DASessionUnscheduleFromRunLoop(session.as_ptr(), run_loop, ffi::kCFRunLoopDefaultMode);
        result
    }
}

#[derive(Debug, Clone, Copy)]
enum Request {
    Unmount,
    Eject,
}

impl Request {
    fn verb(self) -> &'static str {
        match self {
            Request::Unmount => "unmount",
            Request::Eject => "eject",
        }
    }
}

/// What a request's callback reports: `Err` holds why it was refused.
type Outcome = Option<std::result::Result<(), String>>;

/// Asks DiskArbitration to `request` the whole disk `name`, and runs this
/// thread's run loop until it answers. Only called within [`scheduled`].
fn arbitrate(session: &Session, name: &str, request: Request) -> Result<()> {
    let disk = session
        .disk(name)
        .ok_or_else(|| ArbitrationError::NoDisk(name.to_owned()))?;
    let outcome: *mut Outcome = Box::into_raw(Box::new(None));
    // SAFETY: a live disk; `outcome` stays allocated until the callback
    // has written to it (or forever, if it never does).
    unsafe {
        match request {
            Request::Unmount => ffi::DADiskUnmount(
                disk.as_ptr(),
                ffi::kDADiskUnmountOptionWhole,
                Some(answered),
                outcome.cast(),
            ),
            Request::Eject => ffi::DADiskEject(
                disk.as_ptr(),
                ffi::kDADiskEjectOptionDefault,
                Some(answered),
                outcome.cast(),
            ),
        }
        let deadline = Instant::now() + ARBITRATION_TIMEOUT;
        while (*outcome).is_none() && Instant::now() < deadline {
            ffi::CFRunLoopRunInMode(ffi::kCFRunLoopDefaultMode, 0.25, 1);
        }
        match (*outcome).take() {
            // Left allocated: the callback may still come, and write to it.
            None => Err(ArbitrationError::TimedOut {
                disk: name.to_owned(),
                verb: request.verb(),
            }),
            Some(result) => {
                drop(Box::from_raw(outcome));
                result.map_err(|reason| ArbitrationError::Refused {
                    disk: name.to_owned(),
                    verb: request.verb(),
                    reason,
                })
            }
        }
    }
}

extern "C" fn answered(_disk: DADiskRef, dissenter: DADissenterRef, context: *mut c_void) {
    let result = if dissenter.is_null() {
        Ok(())
    } else {
        // SAFETY: a live dissenter, borrowed for the callback's duration.
        let (status, message) = unsafe {
            (
                ffi::DADissenterGetStatus(dissenter),
                ffi::string(ffi::DADissenterGetStatusString(dissenter)),
            )
        };
        match status {
            // Nothing was mounted: as good as unmounted.
            ffi::kDAReturnNotMounted => Ok(()),
            ffi::kDAReturnBusy => {
                Err(message.unwrap_or_else(|| "one of its volumes is in use".to_owned()))
            }
            ffi::kDAReturnNotPrivileged => {
                Err(message.unwrap_or_else(|| "not permitted (run it as root)".to_owned()))
            }
            status => Err(message.unwrap_or_else(|| format!("status {status:#x}"))),
        }
    };
    // SAFETY: `context` is the outcome `arbitrate` allocated, still alive:
    // it waits for this before freeing it.
    unsafe { *context.cast::<Outcome>() = Some(result) };
}

/// Refuses DiskArbitration's mounting any volume of the whole disk it was
/// started for, until dropped: a thread of its own runs the session's run
/// loop, where the approval callback is called.
pub struct MountBlocker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MountBlocker {
    pub fn start(target: &str) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("disk-mount-blocker".to_owned())
            .spawn({
                let target = target.to_owned();
                let stop = stop.clone();
                move || block_mounts(target, &stop, &ready_tx)
            })
            .map_err(|_| ArbitrationError::Session)?;
        if ready_rx.recv() != Ok(true) {
            let _ = thread.join();
            return Err(ArbitrationError::Session);
        }
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for MountBlocker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn block_mounts(target: String, stop: &AtomicBool, ready: &mpsc::Sender<bool>) {
    let Some(session) = Session::new() else {
        let _ = ready.send(false);
        return;
    };
    let context = Box::into_raw(Box::new(target));
    let callback = refuse_mount as ffi::DADiskMountApprovalCallback;
    // SAFETY: `context` outlives the registration (it's freed only after
    // the callback is unregistered), and the session is unscheduled
    // before it's released.
    unsafe {
        ffi::DARegisterDiskMountApprovalCallback(
            session.as_ptr(),
            ptr::null(),
            callback,
            context.cast(),
        );
        let run_loop = ffi::CFRunLoopGetCurrent();
        ffi::DASessionScheduleWithRunLoop(session.as_ptr(), run_loop, ffi::kCFRunLoopDefaultMode);
        let _ = ready.send(true);
        while !stop.load(Ordering::Acquire) {
            let ran = ffi::CFRunLoopRunInMode(ffi::kCFRunLoopDefaultMode, BLOCKER_POLL, 0);
            if ran == ffi::kCFRunLoopRunFinished {
                thread::sleep(Duration::from_secs_f64(BLOCKER_POLL));
            }
        }
        ffi::DASessionUnscheduleFromRunLoop(session.as_ptr(), run_loop, ffi::kCFRunLoopDefaultMode);
        ffi::DAUnregisterApprovalCallback(
            session.as_ptr(),
            callback as *mut c_void,
            context.cast(),
        );
        drop(Box::from_raw(context));
    }
}

/// Dissents from mounting `disk` if it's the target, a partition of it, or
/// a volume of an APFS container on it.
extern "C" fn refuse_mount(disk: DADiskRef, context: *mut c_void) -> DADissenterRef {
    // SAFETY: `context` is the target `block_mounts` registered, alive
    // while it's registered; `disk` is live for the callback's duration.
    let (target, media) = unsafe {
        (
            &*context.cast::<String>(),
            IoObject::owned(ffi::DADiskCopyIOMedia(disk)),
        )
    };
    if !ffi::media_lineage(media).whole_disks.contains(target) {
        return ptr::null();
    }
    let reason = Cf::string("the disk is being flashed");
    // SAFETY: DiskArbitration releases the dissenter it's handed; the
    // dissenter retains its reason.
    unsafe {
        ffi::DADissenterCreate(
            ptr::null(),
            ffi::kDAReturnBusy,
            reason.as_ref().map_or(ptr::null(), Cf::as_ptr),
        )
    }
}
