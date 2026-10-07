use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

use bzip2::write::BzEncoder;
use error_stack::{Report, ResultExt};
use remora_convert::adapter::{ContainerFormatAdapter, Error, Result};
use remora_unpack::Compression;

#[derive(Debug, Default, Clone, Copy)]
pub struct Bzip2AdapterImpl;

impl ContainerFormatAdapter for Bzip2AdapterImpl {
    fn recognizes(&self, header: &[u8]) -> bool {
        Compression::of(header) == Some(Compression::Bzip2)
    }

    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        let unpacked = remora_unpack::open_path(input)
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        let bzip2 = unpacked.compression == Some(Compression::Bzip2);
        if !bzip2 || unpacked.bundled_bmap.is_some() {
            return Err(Report::new(Error::InvalidHeader(input.to_path_buf())));
        }
        let mut outfile = File::create(output_raw)
            .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
        // Every stream, pbzip2's on every core; sparse, as the disk is
        // mostly empty.
        remora_unpack::write_sparse(&mut { unpacked.image }, &mut outfile)
            .map(|_| ())
            .change_context_lazy(|| Error::Decode(input.to_path_buf(), output_raw.to_path_buf()))
    }

    /// One stream, on one core, at `bzip2`'s own default level (9): the
    /// format is kept for images that already use it, zstd is what's fast.
    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        let infile = File::open(input_raw)
            .change_context_lazy(|| Error::ReadFile(input_raw.to_path_buf()))?;
        let mut reader = BufReader::new(infile);
        let outfile =
            File::create(output).change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        let mut encoder = BzEncoder::new(BufWriter::new(outfile), bzip2::Compression::best());
        std::io::copy(&mut reader, &mut encoder)
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        // `finish` writes the stream's end into the BufWriter; flushing that
        // is a separate, equally fallible step, made explicitly: dropping a
        // BufWriter flushes its tail but swallows the error.
        encoder
            .finish()
            .and_then(|mut writer| writer.flush())
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use remora_scratch::ScratchDir;

    use super::*;

    fn image() -> Vec<u8> {
        let mut image = vec![0u8; 1024 * 1024 + 77];
        for (i, b) in image[4096..200_000].iter_mut().enumerate() {
            *b = (i % 253) as u8;
        }
        image
    }

    #[test]
    fn round_trips_an_image() {
        let dir = ScratchDir::new("remora-convert-adapter-bzip2-test").unwrap();
        let raw = dir.join("disk.wic");
        fs::write(&raw, image()).unwrap();

        let bz2 = dir.join("disk.wic.bz2");
        Bzip2AdapterImpl.encode_from_raw(&raw, &bz2).unwrap();
        let back = dir.join("back.wic");
        Bzip2AdapterImpl.decode_to_raw(&bz2, &back).unwrap();

        assert!(Bzip2AdapterImpl.recognizes(&fs::read(&bz2).unwrap()));
        assert_eq!(fs::read(&back).unwrap(), image());
    }

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "requires the bzip2 binary")]
    fn interoperates_with_the_bzip2_binary() {
        // Dev-only real tool, never shelled out to by the shipped binary.
        let dir = ScratchDir::new("remora-convert-adapter-bzip2-test").unwrap();
        let raw = dir.join("disk.wic");
        fs::write(&raw, image()).unwrap();
        let ours = dir.join("ours.wic.bz2");
        Bzip2AdapterImpl.encode_from_raw(&raw, &ours).unwrap();

        let Ok(tested) = Command::new("bzip2").arg("-tq").arg(&ours).status() else {
            eprintln!("skipped: no bzip2 binary");
            return;
        };
        assert!(tested.success(), "bzip2 -t refused our .bz2");

        let status = Command::new("bzip2").arg("-kf").arg(&raw).status().unwrap();
        assert!(status.success());
        let back = dir.join("back.wic");
        Bzip2AdapterImpl
            .decode_to_raw(&dir.join("disk.wic.bz2"), &back)
            .unwrap();
        assert_eq!(fs::read(&back).unwrap(), image());
    }
}
