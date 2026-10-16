//! The few CoreFoundation, DiskArbitration and IOKit calls the macOS
//! backend makes, declared by hand (as their headers have them), plus
//! owning wrappers that release what a `Create`/`Copy` call returned.

#![allow(non_camel_case_types, non_upper_case_globals)]

use std::{
    collections::HashSet,
    ffi::{c_char, c_void, CStr, CString},
    ptr,
};

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFTypeID = usize;
pub type CFIndex = isize;
pub type Boolean = u8;

pub type DASessionRef = *const c_void;
pub type DADiskRef = *const c_void;
pub type DADissenterRef = *const c_void;
pub type DAReturn = i32;

pub type io_object_t = u32;
pub type kern_return_t = i32;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: CFIndex = 4;
pub const kCFRunLoopRunFinished: i32 = 1;

pub const kDADiskUnmountOptionWhole: u32 = 0x0000_0001;
pub const kDADiskEjectOptionDefault: u32 = 0x0000_0000;
pub const kDAReturnBusy: DAReturn = 0xF8DA_0002_u32 as i32;
pub const kDAReturnNotMounted: DAReturn = 0xF8DA_0007_u32 as i32;
pub const kDAReturnNotPrivileged: DAReturn = 0xF8DA_0009_u32 as i32;

pub type DADiskUnmountCallback = extern "C" fn(DADiskRef, DADissenterRef, *mut c_void);
pub type DADiskEjectCallback = DADiskUnmountCallback;
pub type DADiskMountApprovalCallback = extern "C" fn(DADiskRef, *mut c_void) -> DADissenterRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopDefaultMode: CFStringRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetValue(boolean: CFTypeRef) -> Boolean;
    pub fn CFNumberGetValue(number: CFTypeRef, the_type: CFIndex, value: *mut c_void) -> Boolean;
    pub fn CFStringGetLength(string: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    pub fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRunInMode(
        mode: CFStringRef,
        seconds: f64,
        return_after_source_handled: Boolean,
    ) -> i32;
}

#[link(name = "DiskArbitration", kind = "framework")]
extern "C" {
    pub static kDADiskDescriptionMediaSizeKey: CFStringRef;
    pub static kDADiskDescriptionMediaRemovableKey: CFStringRef;
    pub static kDADiskDescriptionMediaEjectableKey: CFStringRef;
    pub static kDADiskDescriptionMediaNameKey: CFStringRef;
    pub static kDADiskDescriptionDeviceModelKey: CFStringRef;

    pub fn DASessionCreate(alloc: CFAllocatorRef) -> DASessionRef;
    pub fn DASessionScheduleWithRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    pub fn DASessionUnscheduleFromRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    pub fn DADiskCreateFromBSDName(
        alloc: CFAllocatorRef,
        session: DASessionRef,
        name: *const c_char,
    ) -> DADiskRef;
    pub fn DADiskCopyDescription(disk: DADiskRef) -> CFDictionaryRef;
    pub fn DADiskCopyIOMedia(disk: DADiskRef) -> io_object_t;
    pub fn DADiskUnmount(
        disk: DADiskRef,
        options: u32,
        callback: Option<DADiskUnmountCallback>,
        context: *mut c_void,
    );
    pub fn DADiskEject(
        disk: DADiskRef,
        options: u32,
        callback: Option<DADiskEjectCallback>,
        context: *mut c_void,
    );
    pub fn DADissenterCreate(
        alloc: CFAllocatorRef,
        status: DAReturn,
        string: CFStringRef,
    ) -> DADissenterRef;
    pub fn DADissenterGetStatus(dissenter: DADissenterRef) -> DAReturn;
    pub fn DADissenterGetStatusString(dissenter: DADissenterRef) -> CFStringRef;
    pub fn DARegisterDiskMountApprovalCallback(
        session: DASessionRef,
        match_: CFDictionaryRef,
        callback: DADiskMountApprovalCallback,
        context: *mut c_void,
    );
    pub fn DAUnregisterApprovalCallback(
        session: DASessionRef,
        callback: *mut c_void,
        context: *mut c_void,
    );
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IORegistryEntryGetParentEntry(
        entry: io_object_t,
        plane: *const c_char,
        parent: *mut io_object_t,
    ) -> kern_return_t;
    pub fn IOObjectConformsTo(object: io_object_t, class_name: *const c_char) -> u32;
    pub fn IORegistryEntryCreateCFProperty(
        entry: io_object_t,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
}

/// A CoreFoundation object this side owns (from a `Create`/`Copy` call),
/// released on drop.
pub struct Cf(CFTypeRef);

impl Cf {
    /// # Safety
    /// `ptr` is null or an object the caller owns a reference to.
    pub unsafe fn owned(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }

    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }

    pub fn string(s: &str) -> Option<Self> {
        let s = CString::new(s).ok()?;
        // SAFETY: a NUL-terminated UTF-8 string; the result is owned.
        unsafe {
            Self::owned(CFStringCreateWithCString(
                ptr::null(),
                s.as_ptr(),
                kCFStringEncodingUTF8,
            ))
        }
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: owned, non-null, released once.
        unsafe { CFRelease(self.0) }
    }
}

/// An IOKit registry entry this side owns, released on drop.
pub struct IoObject(io_object_t);

impl IoObject {
    pub fn owned(object: io_object_t) -> Option<Self> {
        (object != 0).then_some(Self(object))
    }

    /// Its provider in the IOService plane, if it has one.
    pub fn parent(&self) -> Option<Self> {
        let mut parent = 0;
        // SAFETY: a live entry; the parent comes back retained.
        let status =
            unsafe { IORegistryEntryGetParentEntry(self.0, c"IOService".as_ptr(), &mut parent) };
        if status == 0 {
            Self::owned(parent)
        } else {
            None
        }
    }

    pub fn conforms_to(&self, class: &CStr) -> bool {
        // SAFETY: a live entry and a NUL-terminated class name.
        unsafe { IOObjectConformsTo(self.0, class.as_ptr()) != 0 }
    }

    pub fn property(&self, key: &str) -> Option<Cf> {
        let key = Cf::string(key)?;
        // SAFETY: a live entry and a CFString key; the value is owned.
        unsafe {
            Cf::owned(IORegistryEntryCreateCFProperty(
                self.0,
                key.as_ptr(),
                ptr::null(),
                0,
            ))
        }
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: owned, non-zero, released once.
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

/// The value `key` maps to in `dict`, borrowed from it.
///
/// # Safety
/// `dict` is a live CFDictionary.
pub unsafe fn dict_get(dict: &Cf, key: CFStringRef) -> CFTypeRef {
    CFDictionaryGetValue(dict.as_ptr(), key)
}

/// `value` as a Rust string, if it's a CFString.
///
/// # Safety
/// `value` is null or a live CF object.
pub unsafe fn string(value: CFTypeRef) -> Option<String> {
    if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
        return None;
    }
    let len = CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), kCFStringEncodingUTF8);
    let mut buf = vec![0 as c_char; usize::try_from(len).ok()? + 1];
    if CFStringGetCString(
        value,
        buf.as_mut_ptr(),
        buf.len() as CFIndex,
        kCFStringEncodingUTF8,
    ) == 0
    {
        return None;
    }
    Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
}

/// `value` as a bool, if it's a CFBoolean.
///
/// # Safety
/// `value` is null or a live CF object.
pub unsafe fn boolean(value: CFTypeRef) -> Option<bool> {
    (!value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID())
        .then(|| CFBooleanGetValue(value) != 0)
}

/// `value` as a u64, if it's a non-negative CFNumber.
///
/// # Safety
/// `value` is null or a live CF object.
pub unsafe fn number(value: CFTypeRef) -> Option<u64> {
    if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
        return None;
    }
    let mut n: i64 = 0;
    if CFNumberGetValue(value, kCFNumberSInt64Type, (&mut n as *mut i64).cast()) == 0 {
        return None;
    }
    u64::try_from(n).ok()
}

/// A DiskArbitration session, for looking disks up by BSD name.
pub struct Session(Cf);

impl Session {
    pub fn new() -> Option<Self> {
        // SAFETY: the result is owned.
        unsafe { Cf::owned(DASessionCreate(ptr::null())) }.map(Self)
    }

    pub fn as_ptr(&self) -> DASessionRef {
        self.0.as_ptr()
    }

    /// The disk named `bsd_name` (`disk4`), if DiskArbitration knows it.
    pub fn disk(&self, bsd_name: &str) -> Option<Disk> {
        let name = CString::new(bsd_name).ok()?;
        // SAFETY: a live session and a NUL-terminated name; owned result.
        unsafe {
            Cf::owned(DADiskCreateFromBSDName(
                ptr::null(),
                self.as_ptr(),
                name.as_ptr(),
            ))
        }
        .map(Disk)
    }
}

/// A DiskArbitration disk: a whole disk, a partition or a volume.
pub struct Disk(Cf);

impl Disk {
    pub fn as_ptr(&self) -> DADiskRef {
        self.0.as_ptr()
    }

    pub fn description(&self) -> Option<Cf> {
        // SAFETY: a live disk; the description is owned.
        unsafe { Cf::owned(DADiskCopyDescription(self.as_ptr())) }
    }

    /// What the IOKit registry says about this disk and what it's carved
    /// from: see [`media_lineage`].
    pub fn lineage(&self) -> Lineage {
        // SAFETY: a live disk; the media comes back retained.
        media_lineage(IoObject::owned(unsafe { DADiskCopyIOMedia(self.as_ptr()) }))
    }
}

/// Where a disk's media sits in the IOKit registry.
#[derive(Debug, Default)]
pub struct Lineage {
    /// The BSD names of every whole disk on the way up, itself included:
    /// for an APFS volume, its container *and* the physical disk holding
    /// the container's partition.
    pub whole_disks: HashSet<String>,
    /// Some other media is below this one: an APFS container (or a disk
    /// image's) is a view of a partition, not a disk of its own.
    pub synthesized: bool,
}

/// Walks the IOService plane up from `media`: every `IOMedia` crossed is
/// either the media itself, or one it's built on.
pub fn media_lineage(media: Option<IoObject>) -> Lineage {
    let mut lineage = Lineage::default();
    let mut entry = media;
    let mut first = true;
    while let Some(current) = entry {
        if current.conforms_to(c"IOMedia") {
            if !first {
                lineage.synthesized = true;
            }
            // SAFETY: properties come back owned, and are only read here.
            let whole = current
                .property("Whole")
                .and_then(|v| unsafe { boolean(v.as_ptr()) })
                .unwrap_or(false);
            if whole {
                if let Some(name) = current
                    .property("BSD Name")
                    .and_then(|v| unsafe { string(v.as_ptr()) })
                {
                    lineage.whole_disks.insert(name);
                }
            }
        }
        first = false;
        entry = current.parent();
    }
    lineage
}
