use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use bmap_parser::Discarder;
use error_stack::{Report, ResultExt};
use flate2::bufread::MultiGzDecoder;
use remora_flash::adapter::source::{Error, ImageSourceAdapter, ImageStream, Result, SourceImage};

use crate::parallel_bzip2::ParallelBzDecoder;

/// Where a tar header's magic sits: "ustar", in POSIX and GNU tars alike.
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

#[derive(Debug, Default, Clone, Copy)]
pub struct ImageSourceAdapterImpl;

impl ImageSourceAdapter for ImageSourceAdapterImpl {
    fn open(&self, path: &Path) -> Result<SourceImage> {
        let mut file = File::open(path).change_context(Error::Read)?;
        if is_tar(&mut file).change_context(Error::Read)? {
            return open_bundle(file);
        }
        let len = file.metadata().change_context(Error::Read)?.len();
        // A raw image keeps its file's real seeks, to skip unmapped ranges.
        let (stream, raw) =
            image_stream(BufReader::new(file), ImageStream::new).change_context(Error::Read)?;
        Ok(SourceImage {
            stream,
            bundled_bmap: None,
            size: raw.then_some(len),
        })
    }
}

/// Whether `file` starts with a tar header; leaves it rewound either way.
fn is_tar(file: &mut File) -> io::Result<bool> {
    let mut header = Vec::with_capacity(TAR_MAGIC_OFFSET + TAR_MAGIC.len());
    file.by_ref()
        .take(header.capacity() as u64)
        .read_to_end(&mut header)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(header.get(TAR_MAGIC_OFFSET..) == Some(TAR_MAGIC))
}

/// A `.bmaptar`: exactly one `.bmap` and one image, in either order. The
/// image is read where it sits in the tar, never extracted.
fn open_bundle(file: File) -> Result<SourceImage> {
    let mut archive = tar::Archive::new(file);
    let mut bmap = None;
    let mut image = None;
    for entry in archive.entries_with_seek().change_context(Error::Read)? {
        let mut entry = entry.change_context(Error::Read)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let is_bmap = entry
            .path()
            .change_context(Error::Read)?
            .extension()
            .is_some_and(|ext| ext == "bmap");
        if is_bmap {
            if bmap.is_some() {
                return Err(Report::new(Error::Bundle("more than one .bmap")));
            }
            let mut xml = String::new();
            entry.read_to_string(&mut xml).change_context(Error::Read)?;
            bmap = Some(xml);
        } else {
            if image.is_some() {
                return Err(Report::new(Error::Bundle("more than one image")));
            }
            image = Some((entry.raw_file_position(), entry.size()));
        }
    }
    let (offset, size) = image.ok_or_else(|| Report::new(Error::Bundle("no image")))?;
    let bmap = bmap.ok_or_else(|| Report::new(Error::Bundle("no .bmap")))?;

    let mut file = archive.into_inner();
    file.seek(SeekFrom::Start(offset))
        .change_context(Error::Read)?;
    let (stream, raw) = image_stream(BufReader::new(file.take(size)), |raw| {
        ImageStream::new(Discarder::new(raw))
    })
    .change_context(Error::Read)?;
    Ok(SourceImage {
        stream,
        bundled_bmap: Some(bmap),
        size: raw.then_some(size),
    })
}

/// `reader` decompressed when its magic says bzip2 (on every core, see
/// `ParallelBzDecoder`), gzip or zstd, all of them multi-stream; else
/// handed to `raw` as is, and said so.
fn image_stream<R>(
    mut reader: R,
    raw: impl FnOnce(R) -> ImageStream,
) -> io::Result<(ImageStream, bool)>
where
    R: BufRead + Send + 'static,
{
    let magic = reader.fill_buf()?;
    let bzip2 = magic.starts_with(b"BZh");
    let gzip = magic.starts_with(&[0x1f, 0x8b]);
    let zstd = magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]);
    Ok(if bzip2 {
        let decoder = ParallelBzDecoder::new(reader);
        (ImageStream::new(Discarder::new(decoder)), false)
    } else if gzip {
        let decoder = MultiGzDecoder::new(reader);
        (ImageStream::new(Discarder::new(decoder)), false)
    } else if zstd {
        let decoder = zstd::stream::read::Decoder::with_buffer(reader)?;
        (ImageStream::new(Discarder::new(decoder)), false)
    } else {
        (raw(reader), true)
    })
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
        // The image first, as meta-remora's bmaptar orders them.
        bundle(
            &path,
            &[
                ("disk.wic.bz2", &pbzip2(&image())),
                ("disk.wic.bmap", BMAP.as_bytes()),
            ],
        );

        let source = ImageSourceAdapterImpl.open(&path).unwrap();
        assert_eq!(source.bundled_bmap.as_deref(), Some(BMAP));
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
        for (name, data) in [
            ("disk.wic.bz2", pbzip2(&image())),
            ("disk.wic.gz", gzip(&image())),
            ("disk.wic.zst", zstd(&image())),
            ("disk.wic", image()),
        ] {
            let path = dir.path(name);
            fs::write(&path, data).unwrap();

            let source = ImageSourceAdapterImpl.open(&path).unwrap();
            assert!(source.bundled_bmap.is_none(), "{name}");
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
        stream.seek_forward(1000).unwrap();
        let mut byte = [0u8];
        stream.read_exact(&mut byte).unwrap();
        assert_eq!(byte[0], image()[1000]);
        assert_eq!(stream.position(), 1001);
    }
}
