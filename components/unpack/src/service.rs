use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use error_stack::{Report, ResultExt};
use flate2::bufread::MultiGzDecoder;

use crate::{
    error::{Error, Result},
    model::{is_tar, Compression, Image, Unpacked, HEADER_LEN},
    parallel_bzip2::ParallelBzDecoder,
};

/// [`open`] the file at `path`.
pub fn open_path(path: &Path) -> Result<Unpacked<File>> {
    let file = File::open(path).change_context(Error::Read)?;
    let len = file.metadata().change_context(Error::Read)?.len();
    open(file, len)
}

/// A file's image (`len` bytes long): the one its `.bmaptar` bundle holds,
/// or itself, decompressed either way.
pub fn open<F: Read + Seek + Send + 'static>(mut file: F, len: u64) -> Result<Unpacked<F>> {
    if starts_with_tar(&mut file).change_context(Error::Read)? {
        return open_bundle(file);
    }
    let mut reader = BufReader::new(file);
    let compression = Compression::of(reader.fill_buf().change_context(Error::Read)?);
    let image = match compression {
        // A raw image keeps its file's real seeks.
        None => Image::File(reader),
        Some(compression) => {
            Image::Stream(decompress(reader, compression).change_context(Error::Read)?)
        }
    };
    Ok(Unpacked {
        image,
        bundled_bmap: None,
        compression,
        size: compression.is_none().then_some(len),
    })
}

/// Whether `file` starts with a tar header; leaves it rewound either way.
fn starts_with_tar<F: Read + Seek>(file: &mut F) -> io::Result<bool> {
    let mut header = Vec::with_capacity(HEADER_LEN);
    file.by_ref()
        .take(HEADER_LEN as u64)
        .read_to_end(&mut header)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(is_tar(&header))
}

/// A `.bmaptar`: exactly one `.bmap` and one image, in either order
/// (meta-remora puts the `.bmap` first). The image is read where it sits in
/// the tar, never extracted.
fn open_bundle<F: Read + Seek + Send + 'static>(file: F) -> Result<Unpacked<F>> {
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
    let mut reader = BufReader::new(file.take(size));
    let compression = Compression::of(reader.fill_buf().change_context(Error::Read)?);
    let image = match compression {
        None => Box::new(reader),
        Some(compression) => decompress(reader, compression).change_context(Error::Read)?,
    };
    Ok(Unpacked {
        image: Image::Stream(image),
        bundled_bmap: Some(bmap),
        compression,
        size: compression.is_none().then_some(size),
    })
}

/// `reader` decompressed, as `compression` says: bzip2 on every core (see
/// `ParallelBzDecoder`), all of them multi-stream.
fn decompress<R>(reader: R, compression: Compression) -> io::Result<Box<dyn Read + Send>>
where
    R: BufRead + Send + 'static,
{
    Ok(match compression {
        Compression::Bzip2 => Box::new(ParallelBzDecoder::new(reader)),
        Compression::Gzip => Box::new(MultiGzDecoder::new(reader)),
        Compression::Zstd => Box::new(zstd::stream::read::Decoder::with_buffer(reader)?),
    })
}
