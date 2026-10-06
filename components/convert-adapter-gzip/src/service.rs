use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

use error_stack::ResultExt;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use remora_convert::adapter::{ContainerFormatAdapter, Error, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct GzipAdapterImpl;

impl ContainerFormatAdapter for GzipAdapterImpl {
    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        let infile =
            File::open(input).change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        let mut decoder = GzDecoder::new(BufReader::new(infile));
        let outfile = File::create(output_raw)
            .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
        let mut writer = BufWriter::new(outfile);
        std::io::copy(&mut decoder, &mut writer)
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        // Explicitly: dropping a BufWriter flushes its tail but swallows the
        // error, which would leave a truncated image looking decoded.
        writer
            .flush()
            .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))
    }

    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        let infile = File::open(input_raw)
            .change_context_lazy(|| Error::ReadFile(input_raw.to_path_buf()))?;
        let mut reader = BufReader::new(infile);
        let outfile =
            File::create(output).change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        let mut encoder = GzEncoder::new(BufWriter::new(outfile), Compression::default());
        std::io::copy(&mut reader, &mut encoder)
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))?;
        // `finish` writes the gzip trailer into the BufWriter; flushing that
        // is a separate, equally fallible step (see `decode_to_raw`).
        encoder
            .finish()
            .and_then(|mut writer| writer.flush())
            .change_context_lazy(|| Error::WriteFile(output.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-convert-adapter-gzip-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let raw = temp_path("raw");
        let mut f = std::fs::File::create(&raw).unwrap();
        let bytes: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
        f.write_all(&bytes).unwrap();
        drop(f);

        let gz = temp_path("out.gz");
        GzipAdapterImpl.encode_from_raw(&raw, &gz).unwrap();

        let back = temp_path("back");
        GzipAdapterImpl.decode_to_raw(&gz, &back).unwrap();

        assert_eq!(std::fs::read(&raw).unwrap(), std::fs::read(&back).unwrap());

        for p in [raw, gz, back] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "requires the gzip binary")]
    fn decodes_a_real_gzip_produced_by_the_gzip_binary() {
        // Dev-only real tool, never shelled out to by the shipped binary --
        // same posture as other verticals' tests in this workspace.
        let raw = temp_path("raw2");
        std::fs::write(&raw, b"hello from a real gzip binary\n").unwrap();
        let status = std::process::Command::new("gzip")
            .args(["-kf"])
            .arg(&raw)
            .status()
            .expect("gzip not available");
        assert!(status.success());

        let gz_path = PathBuf::from(format!("{}.gz", raw.display()));
        let back = temp_path("back2");
        GzipAdapterImpl.decode_to_raw(&gz_path, &back).unwrap();
        assert_eq!(
            std::fs::read(&back).unwrap(),
            b"hello from a real gzip binary\n"
        );

        for p in [raw, gz_path, back] {
            let _ = std::fs::remove_file(p);
        }
    }
}
