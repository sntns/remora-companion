use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use remora_flash::{
    adapter::release::{ArtifactChunks, ReleaseArtifactAdapterService},
    model::ReleaseArtifact,
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

/// How far ahead a forward seek reads through the download under way
/// rather than starting another one from the new offset: past this, a new
/// call costs less than the bytes it skips.
const SKIP_IN_STREAM: u64 = 4 * 1024 * 1024;

/// How many times a dropped download is picked up again where it stopped,
/// in a row, before the read fails.
const RETRIES: u32 = 3;

/// A release artifact, read like a file: a download from wherever a seek
/// lands, picked up again where it stopped when its connection drops.
/// Blocking (the copy is): runs its downloads on `runtime`, from a thread
/// that isn't one of the runtime's own.
///
/// It keeps the last chunk received, so a seek back into it (a tar reader
/// re-reading a header) costs nothing; any other seek is free until the
/// next read, which then starts a download from there.
pub(crate) struct RemoteFile {
    releases: ReleaseArtifactAdapterService,
    artifact: ReleaseArtifact,
    runtime: Handle,
    cancel: CancellationToken,
    /// Where the next read reads from.
    position: u64,
    /// The download under way, and where its next chunk starts.
    download: Option<(Box<dyn ArtifactChunks>, u64)>,
    /// The last chunk received, and where it starts in the artifact.
    chunk: Vec<u8>,
    chunk_start: u64,
    /// Bytes received over every download, for whoever follows them.
    received: Arc<AtomicU64>,
}

impl RemoteFile {
    pub(crate) fn new(
        releases: ReleaseArtifactAdapterService,
        artifact: ReleaseArtifact,
        runtime: Handle,
        cancel: CancellationToken,
        received: Arc<AtomicU64>,
    ) -> Self {
        Self {
            releases,
            artifact,
            runtime,
            cancel,
            position: 0,
            download: None,
            chunk: Vec::new(),
            chunk_start: 0,
            received,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.artifact.size
    }

    /// The next chunk of the download under way, or of one started from
    /// `from` when none is; one dropped is picked up again where it
    /// stopped, [`RETRIES`] times in a row at most.
    fn next_chunk(&mut self, from: u64) -> io::Result<Option<(Vec<u8>, u64)>> {
        let mut from = from;
        let mut failures = 0;
        loop {
            let attempt = match self.download.take() {
                Some(running) => Ok(running),
                None => self.start(from).map(|download| (download, from)),
            };
            let result =
                attempt.and_then(
                    |(mut download, start)| match self.block_on(download.next()) {
                        Ok(Some(chunk)) => {
                            self.received
                                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                            self.download = Some((download, start + chunk.len() as u64));
                            Ok(Some((chunk, start)))
                        }
                        Ok(None) => Ok(None),
                        Err(error) => {
                            from = start;
                            Err(io::Error::other(format!("{error:?}")))
                        }
                    },
                );
            match result {
                Err(error) if failures < RETRIES && !self.cancel.is_cancelled() => {
                    failures += 1;
                    tracing::debug!("download from byte {from} dropped, resuming: {error}");
                    std::thread::sleep(Duration::from_millis(500 * u64::from(failures)));
                }
                result => return result,
            }
        }
    }

    fn start(&self, from: u64) -> io::Result<Box<dyn ArtifactChunks>> {
        self.block_on(self.releases.download(&self.artifact, from))
            .map_err(|error| io::Error::other(format!("{error:?}")))
    }

    /// Runs `future` to completion on the runtime, or fails it once the
    /// flash is cancelled, so a stalled download doesn't hold Ctrl-C up.
    fn block_on<T>(
        &self,
        future: impl std::future::Future<Output = remora_flash::adapter::release::Result<T>>,
    ) -> remora_flash::adapter::release::Result<T> {
        let cancel = self.cancel.clone();
        self.runtime.block_on(async move {
            tokio::select! {
                result = future => result,
                _ = cancel.cancelled() => Err(error_stack::Report::new(
                    remora_flash::adapter::release::Error::Download("cancelled".into()),
                )),
            }
        })
    }
}

impl Read for RemoteFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.artifact.size {
            return Ok(0);
        }
        loop {
            // From the chunk at hand, when the position is in it.
            let chunk_end = self.chunk_start + self.chunk.len() as u64;
            if (self.chunk_start..chunk_end).contains(&self.position) {
                let at = (self.position - self.chunk_start) as usize;
                let n = buf.len().min(self.chunk.len() - at);
                buf[..n].copy_from_slice(&self.chunk[at..at + n]);
                self.position += n as u64;
                return Ok(n);
            }
            // Else from the download under way, when it's not past the
            // position and not too far behind it; else from a new one.
            let behind = match &self.download {
                Some((_, next)) if *next <= self.position => self.position - next,
                _ => u64::MAX,
            };
            if behind > SKIP_IN_STREAM {
                self.download = None;
            }
            let from = self
                .download
                .as_ref()
                .map_or(self.position, |(_, next)| *next);
            match self.next_chunk(from)? {
                Some((chunk, start)) => {
                    self.chunk = chunk;
                    self.chunk_start = start;
                }
                None => return Ok(0),
            }
        }
    }
}

impl Seek for RemoteFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => self.artifact.size.checked_add_signed(delta),
        };
        self.position = target.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the artifact's start",
            )
        })?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use error_stack::Report;
    use remora_context::model::ContextOverride;
    use remora_flash::{
        adapter::release::{Error, ReleaseArtifactAdapter, Result},
        model::DiskImage,
    };

    use super::*;

    /// Serves `bytes` in `chunk`-sized pieces; the first download fails
    /// after `fail_after` bytes, once. Records every download's offset.
    struct Fake {
        bytes: Vec<u8>,
        chunk: usize,
        fail_after: Mutex<Option<usize>>,
        offsets: Arc<Mutex<Vec<u64>>>,
    }

    struct FakeChunks {
        pieces: std::vec::IntoIter<Result<Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl ArtifactChunks for FakeChunks {
        async fn next(&mut self) -> Result<Option<Vec<u8>>> {
            self.pieces.next().transpose()
        }
    }

    #[async_trait::async_trait]
    impl ReleaseArtifactAdapter for Fake {
        async fn disk_images(
            &self,
            _: Option<&ContextOverride>,
            _: &str,
            _: &str,
        ) -> Result<Vec<DiskImage>> {
            Ok(vec![])
        }

        async fn download(
            &self,
            _: &ReleaseArtifact,
            offset: u64,
        ) -> Result<Box<dyn ArtifactChunks>> {
            self.offsets.lock().unwrap().push(offset);
            let fail_after = self.fail_after.lock().unwrap().take();
            let mut pieces = Vec::new();
            let mut sent = 0;
            for piece in self.bytes[offset as usize..].chunks(self.chunk) {
                if fail_after.is_some_and(|limit| sent + piece.len() > limit) {
                    pieces.push(Err(Report::new(Error::Download("reset".into()))));
                    break;
                }
                sent += piece.len();
                pieces.push(Ok(piece.to_vec()));
            }
            Ok(Box::new(FakeChunks {
                pieces: pieces.into_iter(),
            }))
        }
    }

    fn bytes() -> Vec<u8> {
        (0..100_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn remote(fail_after: Option<usize>) -> (RemoteFile, Arc<Mutex<Vec<u64>>>, Arc<AtomicU64>) {
        let offsets = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            bytes: bytes(),
            chunk: 1000,
            fail_after: Mutex::new(fail_after),
            offsets: offsets.clone(),
        };
        let received = Arc::new(AtomicU64::new(0));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        // The runtime must outlive the file: leak it, it's a test.
        std::mem::forget(runtime);
        let file = RemoteFile::new(
            ReleaseArtifactAdapterService::new(fake),
            ReleaseArtifact {
                over: None,
                release: "r1".into(),
                file_name: "disk.wic.bmaptar".into(),
                size: bytes().len() as u64,
                tag_condition: String::new(),
            },
            handle,
            CancellationToken::new(),
            received.clone(),
        );
        (file, offsets, received)
    }

    #[test]
    fn reads_whole_in_one_download() {
        let (mut file, offsets, received) = remote(None);
        let mut out = Vec::new();
        file.read_to_end(&mut out).unwrap();
        assert_eq!(out, bytes());
        assert_eq!(*offsets.lock().unwrap(), [0]);
        assert_eq!(received.load(Ordering::Relaxed), bytes().len() as u64);
    }

    #[test]
    fn seeks_back_into_the_last_chunk_for_free_and_far_ahead_with_a_new_download() {
        let (mut file, offsets, _) = remote(None);
        let mut head = [0u8; 300];
        file.read_exact(&mut head).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut head).unwrap();
        assert_eq!(head[..], bytes()[..300]);
        // A short hop ahead reads through the download under way.
        file.seek(SeekFrom::Start(5_000)).unwrap();
        let mut byte = [0u8];
        file.read_exact(&mut byte).unwrap();
        assert_eq!(byte[0], bytes()[5_000]);
        // Back before the chunk at hand: a new download from there.
        file.seek(SeekFrom::Start(10)).unwrap();
        file.read_exact(&mut byte).unwrap();
        assert_eq!(byte[0], bytes()[10]);
        assert_eq!(*offsets.lock().unwrap(), [0, 10]);
    }

    #[test]
    fn a_dropped_download_resumes_where_it_stopped() {
        let (mut file, offsets, _) = remote(Some(42_000));
        let mut out = Vec::new();
        file.read_to_end(&mut out).unwrap();
        assert_eq!(out, bytes());
        assert_eq!(*offsets.lock().unwrap(), [0, 42_000]);
    }
}
