use std::{fs, path::Path, sync::Arc};

use error_stack::{Report, ResultExt};
use remora_etcher_convert::{
    adapter::ContainerFormatAdapter,
    application::{ConvertServiceInterface, Error, Result},
};

/// Which container format a path's own extension names -- the only signal
/// this vertical uses to pick an adapter (or none, for a plain raw copy),
/// matching how every caller already names its images (`.qcow2`, `.gz`, or
/// nothing recognized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Raw,
    Qcow2,
    Gzip,
}

fn format_of(path: &Path) -> Format {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("qcow2") => Format::Qcow2,
        Some(ext) if ext.eq_ignore_ascii_case("gz") => Format::Gzip,
        _ => Format::Raw,
    }
}

/// The convert vertical's use case: pick the right `ContainerFormatAdapter`
/// (or none, for a plain raw copy) purely from the path's own extension,
/// and delegate to it.
pub struct ConvertControllerImpl {
    qcow2: Arc<dyn ContainerFormatAdapter>,
    gzip: Arc<dyn ContainerFormatAdapter>,
}

impl ConvertControllerImpl {
    pub fn new(
        qcow2: Arc<dyn ContainerFormatAdapter>,
        gzip: Arc<dyn ContainerFormatAdapter>,
    ) -> Self {
        Self { qcow2, gzip }
    }

    fn adapter_for(&self, format: Format) -> Option<&Arc<dyn ContainerFormatAdapter>> {
        match format {
            Format::Raw => None,
            Format::Qcow2 => Some(&self.qcow2),
            Format::Gzip => Some(&self.gzip),
        }
    }
}

impl ConvertServiceInterface for ConvertControllerImpl {
    fn to_raw(&self, image: &Path, output_raw: &Path) -> Result<()> {
        match self.adapter_for(format_of(image)) {
            None => fs::copy(image, output_raw).map(|_| ()).map_err(|_| {
                Report::new(Error::Copy(image.to_path_buf(), output_raw.to_path_buf()))
            }),
            Some(adapter) => adapter
                .decode_to_raw(image, output_raw)
                .change_context(Error::Decode(image.to_path_buf())),
        }
    }

    fn from_raw(&self, raw_image: &Path, output: &Path) -> Result<()> {
        match self.adapter_for(format_of(output)) {
            None => fs::copy(raw_image, output).map(|_| ()).map_err(|_| {
                Report::new(Error::Copy(raw_image.to_path_buf(), output.to_path_buf()))
            }),
            Some(adapter) => adapter
                .encode_from_raw(raw_image, output)
                .change_context(Error::Encode(output.to_path_buf())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-convert-application-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    fn controller() -> ConvertControllerImpl {
        ConvertControllerImpl::new(
            Arc::new(remora_etcher_convert_adapter_qcow2::Qcow2AdapterImpl),
            Arc::new(remora_etcher_convert_adapter_gzip::GzipAdapterImpl),
        )
    }

    #[test]
    fn round_trips_through_gzip_by_extension() {
        let raw = temp_path("raw");
        let mut f = fs::File::create(&raw).unwrap();
        f.write_all(&[1u8, 2, 3, 4, 5, 0, 0, 0]).unwrap();
        drop(f);

        let gz = temp_path("out.gz");
        controller().from_raw(&raw, &gz).unwrap();

        let back = temp_path("back.raw");
        controller().to_raw(&gz, &back).unwrap();

        assert_eq!(fs::read(&raw).unwrap(), fs::read(&back).unwrap());

        for p in [raw, gz, back] {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn a_plain_raw_path_is_just_copied() {
        let raw = temp_path("raw2");
        fs::write(&raw, b"hello").unwrap();
        let copy = temp_path("copy2.raw");
        controller().from_raw(&raw, &copy).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"hello");
        for p in [raw, copy] {
            let _ = fs::remove_file(p);
        }
    }
}
