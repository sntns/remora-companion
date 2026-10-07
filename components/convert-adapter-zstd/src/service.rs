use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
    thread,
};

use error_stack::{Report, ResultExt};
use remora_convert::adapter::{ContainerFormatAdapter, Error, Result};
use remora_unpack::Compression;

/// oe-core's `ZSTD_COMPRESSION_LEVEL`, which meta-remora's images are
/// compressed at.
const LEVEL: i32 = 3;

#[derive(Debug, Default, Clone, Copy)]
pub struct ZstdAdapterImpl;

impl ContainerFormatAdapter for ZstdAdapterImpl {
    fn recognizes(&self, header: &[u8]) -> bool {
        Compression::of(header) == Some(Compression::Zstd)
    }

    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        let unpacked = remora_unpack::open_path(input)
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        let zstd = unpacked.compression == Some(Compression::Zstd);
        if !zstd || unpacked.bundled_bmap.is_some() {
            return Err(Report::new(Error::InvalidHeader(input.to_path_buf())));
        }
        let mut outfile = File::create(output_raw)
            .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
        // Every frame, as zstd -d reads them; sparse, as it writes them.
        remora_unpack::write_sparse(&mut { unpacked.image }, &mut outfile)
            .map(|_| ())
            .change_context_lazy(|| Error::Decode(input.to_path_buf(), output_raw.to_path_buf()))
    }

    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        let infile = File::open(input_raw)
            .change_context_lazy(|| Error::ReadFile(input_raw.to_path_buf()))?;
        let len = infile
            .metadata()
            .change_context_lazy(|| Error::ReadFile(input_raw.to_path_buf()))?
            .len();
        let outfile =
            File::create(output).change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        let mut writer = BufWriter::new(outfile);
        compress(&mut BufReader::new(infile), len, &mut writer)
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        // Explicitly: dropping a BufWriter flushes its tail but swallows the
        // error, which would leave a truncated file looking encoded.
        writer
            .flush()
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))
    }
}

/// `input` (`len` bytes) as one zstd frame into `output`, the way
/// `zstd -3 --threads=N` writes it: on every core, with the content size
/// and checksum in the frame.
fn compress(
    input: &mut impl std::io::Read,
    len: u64,
    output: &mut impl Write,
) -> std::io::Result<()> {
    let mut encoder = zstd::stream::write::Encoder::new(output, LEVEL)?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    encoder.multithread(threads as u32)?;
    encoder.include_checksum(true)?;
    encoder.set_pledged_src_size(Some(len))?;
    std::io::copy(input, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use remora_scratch::ScratchDir;

    use super::*;

    /// Data and zero runs, a few blocks each, ending mid-block.
    fn image() -> Vec<u8> {
        let mut image = vec![0u8; 3 * 1024 * 1024 + 1000];
        for (i, b) in image[8192..300_000].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        image[2_000_000..2_100_000].fill(0x5a);
        *image.last_mut().unwrap() = 1;
        image
    }

    #[test]
    fn round_trips_an_image() {
        let dir = ScratchDir::new("remora-convert-adapter-zstd-test").unwrap();
        let raw = dir.join("disk.wic");
        fs::write(&raw, image()).unwrap();

        let zst = dir.join("disk.wic.zst");
        ZstdAdapterImpl.encode_from_raw(&raw, &zst).unwrap();
        let back = dir.join("back.wic");
        ZstdAdapterImpl.decode_to_raw(&zst, &back).unwrap();

        assert!(ZstdAdapterImpl.recognizes(&fs::read(&zst).unwrap()));
        assert_eq!(fs::read(&back).unwrap(), image());
    }

    #[test]
    fn a_gzip_is_not_decoded_as_zstd() {
        let dir = ScratchDir::new("remora-convert-adapter-zstd-test").unwrap();
        let gz = dir.join("disk.wic.zst");
        fs::write(&gz, [0x1f, 0x8b, 8, 0, 0, 0, 0, 0]).unwrap();

        let err = ZstdAdapterImpl
            .decode_to_raw(&gz, &dir.join("back.wic"))
            .unwrap_err();
        assert!(matches!(err.current_context(), Error::InvalidHeader(_)));
    }

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "requires the zstd binary")]
    fn interoperates_with_the_zstd_binary() {
        // Dev-only real tool, never shelled out to by the shipped binary.
        let dir = ScratchDir::new("remora-convert-adapter-zstd-test").unwrap();
        let raw = dir.join("disk.wic");
        fs::write(&raw, image()).unwrap();
        let ours = dir.join("ours.wic.zst");
        ZstdAdapterImpl.encode_from_raw(&raw, &ours).unwrap();

        let tested = Command::new("zstd").arg("-tq").arg(&ours).status();
        let Ok(tested) = tested else {
            eprintln!("skipped: no zstd binary");
            return;
        };
        assert!(tested.success(), "zstd -t refused our .zst");

        let theirs = dir.join("theirs.wic.zst");
        let status = Command::new("zstd")
            .args(["-q", "-3", "-T0", "-o"])
            .arg(&theirs)
            .arg(&raw)
            .status()
            .unwrap();
        assert!(status.success());
        let back = dir.join("back.wic");
        ZstdAdapterImpl.decode_to_raw(&theirs, &back).unwrap();
        assert_eq!(fs::read(&back).unwrap(), image());
    }
}
