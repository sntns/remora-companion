use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
};

use bmap_parser::Discarder;
use error_stack::{Report, ResultExt};
use remora_flash::{
    adapter::source::{Error, ImageFile, ImageSourceAdapter, ImageStream, Result, SourceImage},
    model::Compression,
};
use remora_unpack::Image;

#[derive(Debug, Default, Clone, Copy)]
pub struct ImageSourceAdapterImpl;

impl ImageSourceAdapter for ImageSourceAdapterImpl {
    fn open(&self, path: &Path) -> Result<SourceImage> {
        let file = File::open(path).change_context(Error::Read)?;
        let len = file.metadata().change_context(Error::Read)?.len();
        open_any(file, len)
    }

    fn open_file(&self, file: Box<dyn ImageFile>, len: u64) -> Result<SourceImage> {
        open_any(file, len)
    }
}

/// A file's image: the one its `.bmaptar` bundle holds, or itself.
fn open_any<F: Read + Seek + Send + 'static>(file: F, len: u64) -> Result<SourceImage> {
    let unpacked = remora_unpack::open(file, len).map_err(source_error)?;
    let stream = match unpacked.image {
        // A raw image keeps its file's real seeks, to skip unmapped ranges.
        Image::File(file) => ImageStream::new(file),
        Image::Stream(stream) => ImageStream::new(Discarder::new(stream)),
    };
    Ok(SourceImage {
        stream,
        bundle: unpacked.bundled_bmap.is_some(),
        bundled_bmap: unpacked.bundled_bmap,
        compression: unpacked.compression.map(|compression| match compression {
            remora_unpack::Compression::Bzip2 => Compression::Bzip2,
            remora_unpack::Compression::Gzip => Compression::Gzip,
            remora_unpack::Compression::Zstd => Compression::Zstd,
        }),
        size: unpacked.size,
    })
}

/// This port's own error for what unpacking said, the chain kept.
fn source_error(report: Report<remora_unpack::Error>) -> Report<Error> {
    let context = match report.current_context() {
        remora_unpack::Error::Bundle(what) => Error::Bundle(what),
        remora_unpack::Error::Read | remora_unpack::Error::Write => Error::Read,
    };
    report.change_context(context)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicU32, Ordering},
    };

    use bmap_parser::SeekForward;

    use super::*;

    const BMAP: &str = "<bmap/>";

    static FIXTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway directory under the OS temp dir, removed on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let id = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "remora-flash-file-test-{}-{}",
                std::process::id(),
                id
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Recognisable, so a stream read from the wrong offset shows.
    fn image() -> Vec<u8> {
        (0..64 * 1024u32).map(|i| (i % 251) as u8).collect()
    }

    /// bzip2'd as pbzip2 does it: several concatenated streams.
    fn pbzip2(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(data.len() / 3) {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(chunk).unwrap();
            out.extend(encoder.finish().unwrap());
        }
        out
    }

    fn zstd(data: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(data, 3).unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn bundle(path: &Path, entries: &[(&str, &[u8])]) {
        let mut builder = tar::Builder::new(File::create(path).unwrap());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.finish().unwrap();
    }

    fn read_all(mut source: SourceImage) -> Vec<u8> {
        let mut data = Vec::new();
        source.stream.read_to_end(&mut data).unwrap();
        data
    }

    #[test]
    fn a_bmaptar_yields_its_image_decompressed_and_its_bmap() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.bmaptar");
        // As meta-remora's bmaptar holds them: the .bmap, then the zstd'd
        // image.
        bundle(
            &path,
            &[
                ("disk.wic.bmap", BMAP.as_bytes()),
                ("disk.wic.zst", &zstd(&image())),
            ],
        );

        let source = ImageSourceAdapterImpl.open(&path).unwrap();
        assert_eq!(source.bundled_bmap.as_deref(), Some(BMAP));
        assert!(source.bundle);
        assert_eq!(source.compression, Some(Compression::Zstd));
        assert_eq!(read_all(source), image());
    }

    #[test]
    fn a_bmaptar_may_hold_its_image_first() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.bmaptar");
        // The other order (and pbzip2, as Yocto has it) reads the same.
        bundle(
            &path,
            &[
                ("disk.wic.bz2", &pbzip2(&image())),
                ("disk.wic.bmap", BMAP.as_bytes()),
            ],
        );

        let source = ImageSourceAdapterImpl.open(&path).unwrap();
        assert_eq!(source.bundled_bmap.as_deref(), Some(BMAP));
        assert_eq!(source.compression, Some(Compression::Bzip2));
        assert_eq!(read_all(source), image());
    }

    #[test]
    fn a_bmaptar_may_hold_a_raw_image_after_its_bmap() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.bmaptar");
        bundle(
            &path,
            &[("disk.wic.bmap", BMAP.as_bytes()), ("disk.wic", &image())],
        );

        let source = ImageSourceAdapterImpl.open(&path).unwrap();
        assert_eq!(source.bundled_bmap.as_deref(), Some(BMAP));
        assert_eq!(source.size, Some(image().len() as u64));
        assert_eq!(read_all(source), image());
    }

    #[test]
    fn a_bmaptar_without_its_bmap_is_refused() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.bmaptar");
        bundle(&path, &[("disk.wic.bz2", &pbzip2(&image()))]);

        let err = ImageSourceAdapterImpl.open(&path).err().unwrap();
        assert!(matches!(err.current_context(), Error::Bundle("no .bmap")));
    }

    #[test]
    fn a_plain_image_is_read_decompressed_or_as_is() {
        let dir = TempDir::new();
        for (name, data, compression) in [
            ("disk.wic.bz2", pbzip2(&image()), Some(Compression::Bzip2)),
            ("disk.wic.gz", gzip(&image()), Some(Compression::Gzip)),
            ("disk.wic.zst", zstd(&image()), Some(Compression::Zstd)),
            ("disk.wic", image(), None),
        ] {
            let path = dir.path(name);
            fs::write(&path, data).unwrap();

            let source = ImageSourceAdapterImpl.open(&path).unwrap();
            assert!(source.bundled_bmap.is_none(), "{name}");
            assert!(!source.bundle, "{name}");
            assert_eq!(source.compression, compression, "{name}");
            let size = (name == "disk.wic").then_some(image().len() as u64);
            assert_eq!(source.size, size, "{name}");
            assert_eq!(read_all(source), image(), "{name}");
        }
    }

    #[test]
    fn the_stream_tracks_what_was_read_and_skipped() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.bz2");
        fs::write(&path, pbzip2(&image())).unwrap();

        let mut stream = ImageSourceAdapterImpl.open(&path).unwrap().stream;
        let copied = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        stream.follow(copied.clone(), tokio_util::sync::CancellationToken::new());
        stream.seek_forward(1000).unwrap();
        let mut byte = [0u8];
        stream.read_exact(&mut byte).unwrap();
        assert_eq!(byte[0], image()[1000]);
        assert_eq!(stream.position(), 1001);
        assert_eq!(copied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_cancelled_stream_neither_reads_nor_skips() {
        let dir = TempDir::new();
        let path = dir.path("disk.wic.zst");
        fs::write(&path, zstd(&image())).unwrap();

        let mut stream = ImageSourceAdapterImpl.open(&path).unwrap().stream;
        let cancel = tokio_util::sync::CancellationToken::new();
        stream.follow(std::sync::Arc::default(), cancel.clone());
        cancel.cancel();
        assert!(stream.seek_forward(1000).is_err());
        assert!(stream.read(&mut [0u8; 16]).is_err());
        assert_eq!(stream.position(), 0);
    }
}
