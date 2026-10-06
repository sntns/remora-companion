use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use remora_ota::adapter::source::{ArtifactReader, ArtifactSourceAdapter, Error, Result};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Artifacts as local files, through `tokio::fs` (blocking reads on
/// tokio's blocking pool, off the thread driving the upload).
pub struct FileArtifactSourceImpl;

struct FileReader {
    file: tokio::fs::File,
    path: PathBuf,
}

#[async_trait::async_trait]
impl ArtifactSourceAdapter for FileArtifactSourceImpl {
    async fn length(&self, path: &Path) -> Result<u64> {
        let metadata = tokio::fs::metadata(path)
            .await
            .change_context(Error::Open)
            .attach_with(|| path.display().to_string())?;
        if !metadata.is_file() {
            return Err(
                Report::new(Error::Open).attach(format!("{} is not a file", path.display()))
            );
        }
        Ok(metadata.len())
    }

    async fn open(&self, path: &Path, offset: u64) -> Result<Box<dyn ArtifactReader>> {
        let mut file = tokio::fs::File::open(path)
            .await
            .change_context(Error::Open)
            .attach_with(|| path.display().to_string())?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .change_context(Error::Read)
            .attach_with(|| path.display().to_string())?;
        Ok(Box::new(FileReader {
            file,
            path: path.to_owned(),
        }))
    }
}

#[async_trait::async_trait]
impl ArtifactReader for FileReader {
    async fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0; max];
        let mut filled = 0;
        // Fill the whole chunk, so that each message stays a full one
        // whatever size the filesystem hands reads back in.
        while filled < max {
            let read = self
                .file
                .read(&mut buffer[filled..])
                .await
                .change_context(Error::Read)
                .attach_with(|| self.path.display().to_string())?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        buffer.truncate(filled);
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_from_an_offset_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.raucb");
        std::fs::write(&path, b"0123456789").unwrap();

        assert_eq!(FileArtifactSourceImpl.length(&path).await.unwrap(), 10);
        let mut reader = FileArtifactSourceImpl.open(&path, 3).await.unwrap();
        assert_eq!(reader.read(4).await.unwrap(), b"3456");
        assert_eq!(reader.read(4).await.unwrap(), b"789");
        assert!(reader.read(4).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_file_or_a_directory_fails_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let report = FileArtifactSourceImpl
            .length(&dir.path().join("nope"))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Open));
        let report = FileArtifactSourceImpl.length(dir.path()).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Open));
    }
}
