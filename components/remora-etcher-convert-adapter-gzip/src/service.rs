use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
};

use error_stack::Report;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use remora_etcher_convert::adapter::{ContainerFormatAdapter, Error, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct GzipAdapterImpl;

impl ContainerFormatAdapter for GzipAdapterImpl {
    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        let infile =
            File::open(input).map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
        let mut decoder = GzDecoder::new(BufReader::new(infile));
        let outfile = File::create(output_raw)
            .map_err(|_| Report::new(Error::WriteFile(output_raw.to_path_buf())))?;
        let mut writer = BufWriter::new(outfile);
        std::io::copy(&mut decoder, &mut writer)
            .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
        Ok(())
    }

    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        let infile = File::open(input_raw)
            .map_err(|_| Report::new(Error::ReadFile(input_raw.to_path_buf())))?;
        let mut reader = BufReader::new(infile);
        let outfile = File::create(output)
            .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;
        let mut encoder = GzEncoder::new(BufWriter::new(outfile), Compression::default());
        std::io::copy(&mut reader, &mut encoder)
            .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;
        encoder
            .finish()
            .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-convert-adapter-gzip-test-{label}-{}-{}",
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
