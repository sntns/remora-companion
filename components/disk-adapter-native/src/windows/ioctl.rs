//! `DeviceIoControl`, the one way Windows answers anything about a disk or
//! a volume, over a `std` file handle.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io, mem,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE},
    System::IO::DeviceIoControl,
};

/// `path` opened to be asked about only: no access right, so no
/// administrator needed (every query used here is `FILE_ANY_ACCESS`).
pub fn open_for_query(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

/// `path` opened to be written, shared with whoever else has it open (the
/// volumes' own handles, locked by this process).
pub fn open_for_write(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

/// Runs `code` on `file` with `input`, into `output`; the bytes it wrote.
pub fn control(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
    let mut returned = 0;
    // SAFETY: a live handle, and buffers of the lengths given.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            if input.is_empty() {
                ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(returned as usize)
    }
}

/// Runs the input-less `code` on `file`, for no output.
pub fn command(file: &File, code: u32) -> io::Result<()> {
    control(file, code, &[], &mut []).map(drop)
}

/// The output of the input-less `code` on `file`, as a `T`.
pub fn query<T: Default + Copy>(file: &File, code: u32) -> io::Result<T> {
    let mut out = T::default();
    // SAFETY: `T` is plain data (a Win32 struct), viewed as its bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut((&mut out as *mut T).cast::<u8>(), mem::size_of::<T>())
    };
    control(file, code, &[], bytes)?;
    Ok(out)
}

/// `value`'s bytes, to pass a Win32 struct as an ioctl's input.
pub fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: plain data, borrowed for as long as `value`.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), mem::size_of::<T>()) }
}

/// A Rust string from a NUL-terminated UTF-16 buffer.
pub fn from_wide(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    OsString::from_wide(&wide[..end])
        .to_string_lossy()
        .into_owned()
}

/// `s` as a NUL-terminated UTF-16 string.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
