use std::{fs, path::Path, sync::Arc};

use error_stack::{Report, ResultExt};
use remora_convert::{
    adapter::ContainerFormatAdapter,
    application::{ConvertServiceInterface, Error, Result},
};
use remora_progress::{track_output_file_size, OperationContext, TrackedOutcome};

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

#[async_trait::async_trait]
impl ConvertServiceInterface for ConvertControllerImpl {
    async fn to_raw(&self, image: &Path, output_raw: &Path, ctx: &OperationContext) -> Result<()> {
        match self.adapter_for(format_of(image)) {
            None => copy_raw(image, output_raw, ctx).await,
            Some(adapter) => {
                ctx.sink.phase("decoding");
                let adapter = adapter.clone();
                // Unlike the encode direction (below), the *compressed*
                // input's size is not a usable "total" here -- the decoded
                // output is normally larger than it, sometimes by a lot, so
                // `done` would blow past `total` almost immediately. Not
                // worth teaching every adapter to expose its decoded size
                // up front just for this: report unknown-total progress
                // instead (see `OperationEvent::Progress`'s doc comment).
                let image_for_work = image.to_path_buf();
                let output_for_work = output_raw.to_path_buf();
                let image = image.to_path_buf();
                match track_output_file_size(ctx, output_raw.to_path_buf(), 0, move || {
                    adapter.decode_to_raw(&image_for_work, &output_for_work)
                })
                .await
                {
                    TrackedOutcome::Completed(res) => res.change_context(Error::Decode(image)),
                    TrackedOutcome::Cancelled => Err(Report::new(Error::Cancelled)),
                }
            }
        }
    }

    async fn from_raw(
        &self,
        raw_image: &Path,
        output: &Path,
        ctx: &OperationContext,
    ) -> Result<()> {
        match self.adapter_for(format_of(output)) {
            None => copy_raw(raw_image, output, ctx).await,
            Some(adapter) => {
                ctx.sink.phase("encoding");
                let adapter = adapter.clone();
                // Only a progress total: 0 reads as "unknown".
                let input_size = fs::metadata(raw_image).map(|m| m.len()).unwrap_or(0);
                let raw_image_for_work = raw_image.to_path_buf();
                let output_for_work = output.to_path_buf();
                let output = output.to_path_buf();
                match track_output_file_size(ctx, output.clone(), input_size, move || {
                    adapter.encode_from_raw(&raw_image_for_work, &output_for_work)
                })
                .await
                {
                    TrackedOutcome::Completed(res) => res.change_context(Error::Encode(output)),
                    TrackedOutcome::Cancelled => Err(Report::new(Error::Cancelled)),
                }
            }
        }
    }
}

/// A plain copy, for a raw image on both sides: multi-gigabyte, so tracked
/// (and cancellable) like a codec run rather than one blocking `fs::copy`
/// on a runtime thread.
async fn copy_raw(from: &Path, to: &Path, ctx: &OperationContext) -> Result<()> {
    ctx.sink.phase("copying");
    // Only a progress total: 0 reads as "unknown".
    let size = fs::metadata(from).map(|m| m.len()).unwrap_or(0);
    let (from, to) = (from.to_path_buf(), to.to_path_buf());
    let (from_for_work, to_for_work) = (from.clone(), to.clone());
    match track_output_file_size(ctx, to.clone(), size, move || {
        fs::copy(&from_for_work, &to_for_work)
    })
    .await
    {
        TrackedOutcome::Completed(res) => res.map(|_| ()).change_context(Error::Copy(from, to)),
        TrackedOutcome::Cancelled => Err(Report::new(Error::Cancelled)),
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
            "remora-convert-application-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    fn controller() -> ConvertControllerImpl {
        ConvertControllerImpl::new(
            Arc::new(remora_convert_adapter_qcow2::Qcow2AdapterImpl),
            Arc::new(remora_convert_adapter_gzip::GzipAdapterImpl),
        )
    }

    #[tokio::test]
    async fn round_trips_through_gzip_by_extension() {
        let raw = temp_path("raw");
        let mut f = fs::File::create(&raw).unwrap();
        f.write_all(&[1u8, 2, 3, 4, 5, 0, 0, 0]).unwrap();
        drop(f);

        let gz = temp_path("out.gz");
        controller()
            .from_raw(&raw, &gz, &OperationContext::noop())
            .await
            .unwrap();

        let back = temp_path("back.raw");
        controller()
            .to_raw(&gz, &back, &OperationContext::noop())
            .await
            .unwrap();

        assert_eq!(fs::read(&raw).unwrap(), fs::read(&back).unwrap());

        for p in [raw, gz, back] {
            let _ = fs::remove_file(p);
        }
    }

    #[tokio::test]
    async fn a_plain_raw_path_is_just_copied() {
        let raw = temp_path("raw2");
        fs::write(&raw, b"hello").unwrap();
        let copy = temp_path("copy2.raw");
        controller()
            .from_raw(&raw, &copy, &OperationContext::noop())
            .await
            .unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"hello");
        for p in [raw, copy] {
            let _ = fs::remove_file(p);
        }
    }

    #[tokio::test]
    async fn reports_progress_while_encoding() {
        let raw = temp_path("raw3");
        fs::write(&raw, vec![0xABu8; 4096]).unwrap();
        let qcow2 = temp_path("out3.qcow2");

        let (sink, mut stream) = remora_progress::channel();
        let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
        controller().from_raw(&raw, &qcow2, &ctx).await.unwrap();
        drop(ctx);

        let mut saw_phase = false;
        while let Some(event) = tokio_stream::StreamExt::next(&mut stream).await {
            if matches!(event, remora_progress::OperationEvent::Phase(_)) {
                saw_phase = true;
            }
        }
        assert!(saw_phase, "expected at least a Phase event");

        for p in [raw, qcow2] {
            let _ = fs::remove_file(p);
        }
    }
}
