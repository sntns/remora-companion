use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use error_stack::{Report, ResultExt};
use remora_convert::{
    adapter::{self, ContainerFormatAdapter, HEADER_LEN},
    application::{ConvertServiceInterface, Error, Result},
};
use remora_progress::{track_output_file_size, OperationContext, TrackedOutcome};
use remora_scratch::ScratchDir;

/// Which container format a path's own extension names -- what picks an
/// adapter (or none, for a plain raw copy), matching how every caller
/// already names its images (`.qcow2`, `.gz`, `.zst`, `.bz2`, `.bmaptar`,
/// or nothing recognized). A file read is also checked against it, by its
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Raw,
    Qcow2,
    Gzip,
    Zstd,
    Bzip2,
    Bmaptar,
}

impl Format {
    const CONTAINERS: [Self; 5] = [
        Self::Qcow2,
        Self::Gzip,
        Self::Zstd,
        Self::Bzip2,
        Self::Bmaptar,
    ];

    fn of(path: &Path) -> Self {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return Self::Raw;
        };
        Self::CONTAINERS
            .into_iter()
            .find(|format| ext.eq_ignore_ascii_case(format.extension()))
            .unwrap_or(Self::Raw)
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Qcow2 => "qcow2",
            Self::Gzip => "gz",
            Self::Zstd => "zst",
            Self::Bzip2 => "bz2",
            Self::Bmaptar => "bmaptar",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Qcow2 => "qcow2",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
            Self::Bzip2 => "bzip2",
            Self::Bmaptar => "bmaptar",
        }
    }
}

/// One adapter per container format, all behind the same port (see
/// `ContainerFormatAdapter` for why they're injected directly).
pub struct ContainerFormats {
    pub qcow2: Arc<dyn ContainerFormatAdapter>,
    pub gzip: Arc<dyn ContainerFormatAdapter>,
    pub zstd: Arc<dyn ContainerFormatAdapter>,
    pub bzip2: Arc<dyn ContainerFormatAdapter>,
    pub bmaptar: Arc<dyn ContainerFormatAdapter>,
}

/// The convert vertical's use case: pick the right `ContainerFormatAdapter`
/// (or none, for a plain raw copy) from the path's own extension, check the
/// file read is what that says, and delegate to it.
///
/// No codec reports progress itself: the file being written is watched
/// growing instead (`track_output_file_size`), against the decoded size
/// when the format says it up front, the raw image's when encoding. It's
/// written next to its destination and renamed into place once complete,
/// so a failed or cancelled run never leaves a truncated file passing for
/// a converted one.
pub struct ConvertControllerImpl {
    formats: ContainerFormats,
}

impl ConvertControllerImpl {
    pub fn new(formats: ContainerFormats) -> Self {
        Self { formats }
    }

    fn adapter_for(&self, format: Format) -> Option<&Arc<dyn ContainerFormatAdapter>> {
        match format {
            Format::Raw => None,
            Format::Qcow2 => Some(&self.formats.qcow2),
            Format::Gzip => Some(&self.formats.gzip),
            Format::Zstd => Some(&self.formats.zstd),
            Format::Bzip2 => Some(&self.formats.bzip2),
            Format::Bmaptar => Some(&self.formats.bmaptar),
        }
    }

    /// The format whose adapter recognizes `header`; `Raw` when none does.
    fn recognize(&self, header: &[u8]) -> Format {
        Format::CONTAINERS
            .into_iter()
            .find(|&format| {
                self.adapter_for(format)
                    .is_some_and(|adapter| adapter.recognizes(header))
            })
            .unwrap_or(Format::Raw)
    }
}

#[async_trait::async_trait]
impl ConvertServiceInterface for ConvertControllerImpl {
    async fn to_raw(&self, image: &Path, output_raw: &Path, ctx: &OperationContext) -> Result<()> {
        let named = Format::of(image);
        let header = read_header(image).await?;
        match (named, self.recognize(&header)) {
            (named, found) if named == found => {}
            (Format::Raw, found) => {
                return Err(Report::new(Error::NotRaw(
                    image.to_path_buf(),
                    found.name(),
                    found.extension(),
                )))
            }
            (named, Format::Raw) => {
                return Err(Report::new(Error::NotFormat(
                    image.to_path_buf(),
                    named.name(),
                )))
            }
            (named, found) => {
                return Err(Report::new(Error::Mismatch(
                    image.to_path_buf(),
                    named.name(),
                    found.name(),
                )))
            }
        }
        let Some(adapter) = self.adapter_for(named) else {
            return copy_raw(image, output_raw, ctx).await;
        };

        ctx.sink.phase("decoding");
        // The decoded size, when the format says it up front, is the total;
        // the compressed input's isn't one (the output is normally larger,
        // sometimes by a lot): unknown, then.
        let total = {
            let (adapter, input) = (adapter.clone(), image.to_path_buf());
            tokio::task::spawn_blocking(move || adapter.decoded_size(&input))
                .await
                .expect("reading a decoded size panicked")
                .change_context_lazy(|| Error::Decode(image.to_path_buf()))?
                .unwrap_or(0)
        };
        let (adapter, input) = (adapter.clone(), image.to_path_buf());
        tracked(ctx, output_raw, total, move |staged| {
            adapter.decode_to_raw(&input, staged)
        })
        .await?
        .change_context_lazy(|| Error::Decode(image.to_path_buf()))
    }

    async fn from_raw(
        &self,
        raw_image: &Path,
        output: &Path,
        ctx: &OperationContext,
    ) -> Result<()> {
        let header = read_header(raw_image).await?;
        let found = self.recognize(&header);
        if found != Format::Raw {
            return Err(Report::new(Error::NotRaw(
                raw_image.to_path_buf(),
                found.name(),
                found.extension(),
            )));
        }
        let Some(adapter) = self.adapter_for(Format::of(output)) else {
            return copy_raw(raw_image, output, ctx).await;
        };

        ctx.sink.phase("encoding");
        // Only a progress total: 0 reads as "unknown".
        let input_size = fs::metadata(raw_image).map(|m| m.len()).unwrap_or(0);
        let (adapter, input) = (adapter.clone(), raw_image.to_path_buf());
        tracked(ctx, output, input_size, move |staged| {
            adapter.encode_from_raw(&input, staged)
        })
        .await?
        .change_context_lazy(|| Error::Encode(output.to_path_buf()))
    }
}

/// A plain copy, for a raw image on both sides: multi-gigabyte, so tracked
/// (and cancellable) like a codec run rather than one blocking `fs::copy`
/// on a runtime thread.
async fn copy_raw(from: &Path, to: &Path, ctx: &OperationContext) -> Result<()> {
    ctx.sink.phase("copying");
    // Only a progress total: 0 reads as "unknown".
    let size = fs::metadata(from).map(|m| m.len()).unwrap_or(0);
    let input = from.to_path_buf();
    tracked(ctx, to, size, move |staged| {
        fs::copy(&input, staged)
            .map(|_| ())
            .change_context_lazy(|| adapter::Error::WriteFile(staged.to_path_buf()))
    })
    .await?
    .change_context_lazy(|| Error::Copy(from.to_path_buf(), to.to_path_buf()))
}

/// The first [`HEADER_LEN`] bytes of `path` (fewer for a shorter file).
async fn read_header(path: &Path) -> Result<Vec<u8>> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> io::Result<Vec<u8>> {
        let mut header = Vec::with_capacity(HEADER_LEN);
        fs::File::open(&owned)?
            .take(HEADER_LEN as u64)
            .read_to_end(&mut header)?;
        Ok(header)
    })
    .await
    .expect("reading a header panicked")
    .change_context_lazy(|| Error::Read(path.to_path_buf()))
}

/// Run `work`, writing `output` at the path it's handed (see [`Staged`]),
/// on a blocking thread, its progress tracked; then put the file in place,
/// unless `ctx` was cancelled meanwhile. `Err`: the error to report as is
/// (cancellation), `Ok(Err)`: the work's own, for the caller to put in its
/// context.
async fn tracked<F>(
    ctx: &OperationContext,
    output: &Path,
    total: u64,
    work: F,
) -> std::result::Result<adapter::Result<()>, Report<Error>>
where
    F: FnOnce(&Path) -> adapter::Result<()> + Send + 'static,
{
    let write_err = || adapter::Error::WriteFile(output.to_path_buf());
    let staged = match Staged::new(output).change_context_lazy(write_err) {
        Ok(staged) => staged,
        Err(e) => return Ok(Err(e)),
    };
    let watched = staged.path.clone();
    let cancel = ctx.cancel.clone();
    let outcome = track_output_file_size(ctx, watched, total, move || {
        work(&staged.path)?;
        if cancel.is_cancelled() {
            // Nobody waits for it any more: dropped, not put in place.
            return Ok(());
        }
        let output = staged.output.clone();
        staged
            .commit()
            .change_context_lazy(|| adapter::Error::WriteFile(output))
    })
    .await;
    match outcome {
        TrackedOutcome::Completed(res) => {
            if res.is_ok() && total > 0 {
                // Done: whatever the last look at the file said, which a
                // quick run may not have had time for.
                ctx.sink.progress(total, total);
            }
            Ok(res)
        }
        TrackedOutcome::Cancelled => Err(Report::new(Error::Cancelled)),
    }
}

/// Where a conversion writes `output`: a file of the same name in a scratch
/// directory next to it, renamed over it once complete; the directory goes
/// with whatever is left in it on every other way out. `output` written in
/// place when it's something other than a regular file (a disk).
struct Staged {
    path: PathBuf,
    output: PathBuf,
    scratch: Option<ScratchDir>,
}

impl Staged {
    fn new(output: &Path) -> io::Result<Self> {
        let in_place = Self {
            path: output.to_path_buf(),
            output: output.to_path_buf(),
            scratch: None,
        };
        let not_a_file = fs::metadata(output).is_ok_and(|m| !m.is_file());
        let Some(name) = output.file_name().filter(|_| !not_a_file) else {
            return Ok(in_place);
        };
        let parent = match output.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let scratch = ScratchDir::new_in(parent, &format!(".{}.partial", name.to_string_lossy()))?;
        Ok(Self {
            path: scratch.join(name),
            output: output.to_path_buf(),
            scratch: Some(scratch),
        })
    }

    fn commit(self) -> io::Result<()> {
        if self.scratch.is_some() {
            fs::rename(&self.path, &self.output)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        // The label last: it carries the extension that names a format.
        path.push(format!(
            "remora-convert-application-test-{}-{}-{label}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    fn controller() -> ConvertControllerImpl {
        ConvertControllerImpl::new(ContainerFormats {
            qcow2: Arc::new(remora_convert_adapter_qcow2::Qcow2AdapterImpl),
            gzip: Arc::new(remora_convert_adapter_gzip::GzipAdapterImpl),
            zstd: Arc::new(remora_convert_adapter_zstd::ZstdAdapterImpl),
            bzip2: Arc::new(remora_convert_adapter_bzip2::Bzip2AdapterImpl),
            bmaptar: Arc::new(remora_convert_adapter_bmaptar::BmaptarAdapterImpl),
        })
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

    /// A raw image with data, holes, and a cut-short last block.
    fn sparse_raw(path: &Path) -> Vec<u8> {
        let mut image = vec![0u8; 3 * 1024 * 1024 + 100];
        image[..512].fill(0x55);
        image[1_000_000..1_200_000].fill(0xa5);
        image[3 * 1024 * 1024 + 99] = 1;
        let mut file = fs::File::create(path).unwrap();
        for (at, chunk) in image.chunks(4096).enumerate() {
            if chunk.iter().any(|&b| b != 0) {
                file.seek(io::SeekFrom::Start(at as u64 * 4096)).unwrap();
                file.write_all(chunk).unwrap();
            }
        }
        file.set_len(image.len() as u64).unwrap();
        image
    }

    #[tokio::test]
    async fn round_trips_through_every_format_by_extension() {
        let raw = temp_path("raw4");
        let image = sparse_raw(&raw);
        for ext in ["qcow2", "gz", "zst", "bz2", "wic.bmaptar"] {
            let packaged = temp_path("out4").with_extension(ext);
            controller()
                .from_raw(&raw, &packaged, &OperationContext::noop())
                .await
                .unwrap();
            let back = temp_path("back4.raw");
            controller()
                .to_raw(&packaged, &back, &OperationContext::noop())
                .await
                .unwrap();
            assert_eq!(fs::read(&back).unwrap(), image, "{ext}");
            for p in [packaged, back] {
                let _ = fs::remove_file(p);
            }
        }
        let _ = fs::remove_file(raw);
    }

    #[tokio::test]
    async fn a_compressed_image_named_as_raw_is_refused_not_copied() {
        let raw = temp_path("raw5");
        sparse_raw(&raw);
        let zst = temp_path("out5.zst");
        controller()
            .from_raw(&raw, &zst, &OperationContext::noop())
            .await
            .unwrap();
        let unnamed = temp_path("unnamed5.img");
        fs::rename(&zst, &unnamed).unwrap();

        let output = temp_path("back5.raw");
        let err = controller()
            .to_raw(&unnamed, &output, &OperationContext::noop())
            .await
            .unwrap_err();

        assert!(
            matches!(err.current_context(), Error::NotRaw(_, "zstd", "zst")),
            "{err:?}"
        );
        assert_eq!(
            err.current_context().to_string(),
            format!(
                "{} holds zstd data, not a raw image: name it *.zst to convert it",
                unnamed.display()
            )
        );
        assert!(!output.exists());

        // Nor encoded as if it were raw.
        let err = controller()
            .from_raw(&unnamed, &temp_path("out5.gz"), &OperationContext::noop())
            .await
            .unwrap_err();
        assert!(matches!(
            err.current_context(),
            Error::NotRaw(_, "zstd", "zst")
        ));

        for p in [raw, unnamed] {
            let _ = fs::remove_file(p);
        }
    }

    #[tokio::test]
    async fn an_image_not_what_its_name_says_is_refused() {
        let raw = temp_path("raw6");
        sparse_raw(&raw);
        let gz = temp_path("out6.gz");
        controller()
            .from_raw(&raw, &gz, &OperationContext::noop())
            .await
            .unwrap();
        let misnamed = temp_path("misnamed6.wic.bmaptar");
        fs::rename(&gz, &misnamed).unwrap();
        let output = temp_path("back6.raw");

        let err = controller()
            .to_raw(&misnamed, &output, &OperationContext::noop())
            .await
            .unwrap_err();
        assert!(
            matches!(err.current_context(), Error::Mismatch(_, "bmaptar", "gzip")),
            "{err:?}"
        );

        // A raw image named as a compressed one.
        let named_zst = temp_path("raw6.zst");
        fs::copy(&raw, &named_zst).unwrap();
        let err = controller()
            .to_raw(&named_zst, &output, &OperationContext::noop())
            .await
            .unwrap_err();
        assert!(
            matches!(err.current_context(), Error::NotFormat(_, "zstd")),
            "{err:?}"
        );
        assert!(!output.exists());

        for p in [raw, misnamed, named_zst] {
            let _ = fs::remove_file(p);
        }
    }

    #[tokio::test]
    async fn decoding_a_bmaptar_counts_up_to_its_image_size() {
        let raw = temp_path("raw7");
        let image = sparse_raw(&raw);
        let bundle = temp_path("out7.wic.bmaptar");
        controller()
            .from_raw(&raw, &bundle, &OperationContext::noop())
            .await
            .unwrap();

        let (sink, mut stream) = remora_progress::channel();
        let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
        let back = temp_path("back7.raw");
        controller().to_raw(&bundle, &back, &ctx).await.unwrap();
        drop(ctx);

        let mut last = None;
        while let Some(event) = tokio_stream::StreamExt::next(&mut stream).await {
            if let remora_progress::OperationEvent::Progress { done, total, .. } = event {
                assert_eq!(total, image.len() as u64);
                last = Some(done);
            }
        }
        assert_eq!(last, Some(image.len() as u64));

        for p in [raw, bundle, back] {
            let _ = fs::remove_file(p);
        }
    }

    #[tokio::test]
    async fn a_failed_encode_leaves_nothing_behind() {
        let raw = temp_path("raw8");
        fs::write(&raw, b"").unwrap();
        let dir = temp_path("dir8");
        fs::create_dir(&dir).unwrap();
        let bundle = dir.join("empty.wic.bmaptar");

        let err = controller()
            .from_raw(&raw, &bundle, &OperationContext::noop())
            .await
            .unwrap_err();
        assert!(matches!(err.current_context(), Error::Encode(_)), "{err:?}");

        // Neither the output nor its scratch directory.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_file(raw);
        let _ = fs::remove_dir_all(dir);
    }
}
