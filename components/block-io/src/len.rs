use std::{
    fs::File,
    io::{self, Seek, SeekFrom},
    path::Path,
};

/// `file`'s length in bytes, a disk device's included. Its position is
/// left where it was.
pub fn len(file: &File) -> io::Result<u64> {
    #[cfg(target_os = "macos")]
    if let Some(len) = crate::macos::device_len(file)? {
        return Ok(len);
    }
    #[cfg(windows)]
    if let Some(len) = crate::windows::device_len(file) {
        return Ok(len);
    }
    let mut file = file;
    let at = file.stream_position()?;
    let end = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(at))?;
    Ok(end)
}

/// The length of the file or device at `path`.
pub fn len_of(path: &Path) -> io::Result<u64> {
    len(&File::open(path)?)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn a_files_length_leaves_its_position_alone() {
        let path = std::env::temp_dir().join(format!("remora-block-io-len-{}", std::process::id()));
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&[7; 1000]).unwrap();
        file.seek(SeekFrom::Start(10)).unwrap();
        assert_eq!(len(&file).unwrap(), 1000);
        assert_eq!(file.stream_position().unwrap(), 10);
        assert_eq!(len_of(&path).unwrap(), 1000);
        std::fs::remove_file(path).unwrap();
    }
}
