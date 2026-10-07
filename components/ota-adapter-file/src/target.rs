use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_ota::adapter::target::{ArtifactTargetAdapter, ArtifactWriter, Error, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Downloads as local files, through `tokio::fs`: written to
/// `<file>.part` next to their destination, renamed over it once whole.
pub struct FileArtifactTargetImpl;

/// The partial file a download to `path` is written to.
fn part_of(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".part");
    PathBuf::from(name)
}

struct FileWriter {
    file: tokio::fs::File,
    path: PathBuf,
    hasher: Sha256,
}

#[async_trait::async_trait]
impl ArtifactTargetAdapter for FileArtifactTargetImpl {
    async fn exists(&self, path: &Path) -> Result<bool> {
        tokio::fs::try_exists(path)
            .await
            .change_context(Error::Open)
            .attach_with(|| path.display().to_string())
    }

    async fn partial(&self, path: &Path) -> Result<u64> {
        match tokio::fs::metadata(part_of(path)).await {
            Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
            Ok(_) => Err(Report::new(Error::Open)
                .attach(format!("{} is not a file", part_of(path).display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(Report::new(error)
                .change_context(Error::Open)
                .attach(part_of(path).display().to_string())),
        }
    }

    async fn open(&self, path: &Path, offset: u64) -> Result<Box<dyn ArtifactWriter>> {
        let part = part_of(path);
        let open_error = || part.display().to_string();
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&part)
            .await
            .change_context(Error::Open)
            .attach_with(open_error)?;
        file.set_len(offset)
            .await
            .change_context(Error::Write)
            .attach_with(open_error)?;
        // What an earlier run wrote counts in the file's checksum.
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .await
                .change_context(Error::Open)
                .attach_with(open_error)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Box::new(FileWriter {
            file,
            path: part,
            hasher,
        }))
    }

    async fn commit(&self, path: &Path) -> Result<()> {
        tokio::fs::rename(part_of(path), path)
            .await
            .change_context(Error::Write)
            .attach_with(|| path.display().to_string())
    }

    async fn discard(&self, path: &Path) -> Result<()> {
        tokio::fs::remove_file(part_of(path))
            .await
            .change_context(Error::Write)
            .attach_with(|| part_of(path).display().to_string())
    }
}

#[async_trait::async_trait]
impl ArtifactWriter for FileWriter {
    async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        self.hasher.update(chunk);
        self.file
            .write_all(chunk)
            .await
            .change_context(Error::Write)
            .attach_with(|| self.path.display().to_string())
    }

    async fn finish(mut self: Box<Self>) -> Result<String> {
        self.file
            .sync_all()
            .await
            .change_context(Error::Write)
            .attach_with(|| self.path.display().to_string())?;
        Ok(self
            .hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[tokio::test]
    async fn a_download_resumes_its_partial_file_and_hashes_it_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.wic.bmaptar");
        let target = FileArtifactTargetImpl;
        assert_eq!(target.partial(&path).await.unwrap(), 0);

        let mut writer = target.open(&path, 0).await.unwrap();
        writer.write(b"hello ").await.unwrap();
        writer.write(b"wor").await.unwrap();
        drop(writer);
        assert_eq!(target.partial(&path).await.unwrap(), 9);
        assert!(!target.exists(&path).await.unwrap());

        // Picked up again from 6: what lies past it is written over.
        let mut writer = target.open(&path, 6).await.unwrap();
        writer.write(b"world").await.unwrap();
        assert_eq!(writer.finish().await.unwrap(), sha256(b"hello world"));
        target.commit(&path).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        assert_eq!(target.partial(&path).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_discarded_download_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        let target = FileArtifactTargetImpl;
        let mut writer = target.open(&path, 0).await.unwrap();
        writer.write(b"junk").await.unwrap();
        writer.finish().await.unwrap();
        target.discard(&path).await.unwrap();
        assert_eq!(target.partial(&path).await.unwrap(), 0);
        assert!(!target.exists(&path).await.unwrap());
    }
}
