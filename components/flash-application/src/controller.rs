use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use error_stack::{Report, ResultExt};
use remora_context::model::ContextOverride;
use remora_disk::{application::DiskService, model::DiskInfo};
use remora_flash::{
    adapter::{
        bmap::{BlockMap, BmapAdapterService},
        release::ReleaseArtifactAdapterService,
        source::{ImageSourceAdapterService, SourceImage},
    },
    application::{Error, FlashServiceInterface, Result},
    model::{
        BmapOrigin, BmapSource, BmapSummary, DiskImage, FlashOutcome, FlashRequest, ImageOrigin,
        ImageSummary,
    },
};
use remora_format::human_size;
use remora_progress::{OperationContext, ProgressSink};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::remote::RemoteFile;

/// The flash vertical's use case. The actual safety guard (`preflight`) lives
/// here, not in the adapter — so nothing, CLI or a future GUI, can reach the
/// raw copy without going through it first. It asks the disk vertical about
/// the target itself rather than trusting a description of it.
pub struct FlashControllerImpl {
    disk: DiskService,
    readers: Readers,
}

/// What an image is read with: its bytes (a local file's, or a release
/// artifact's), its compression or bundle, and its `.bmap`. Cloned into
/// the blocking threads that read it.
#[derive(Clone)]
struct Readers {
    images: ImageSourceAdapterService,
    bmap: BmapAdapterService,
    releases: ReleaseArtifactAdapterService,
}

/// What reading a release artifact downloads with: the runtime the
/// downloads run on, what stops them, and where they count their bytes.
struct Fetch {
    runtime: Handle,
    cancel: CancellationToken,
    received: Arc<AtomicU64>,
}

impl Fetch {
    /// For a read that nobody follows nor cancels: an inspection.
    fn unfollowed() -> Self {
        Self {
            runtime: Handle::current(),
            cancel: CancellationToken::new(),
            received: Arc::default(),
        }
    }
}

impl FlashControllerImpl {
    pub fn new(
        disk: DiskService,
        images: ImageSourceAdapterService,
        bmap: BmapAdapterService,
        releases: ReleaseArtifactAdapterService,
    ) -> Self {
        Self {
            disk,
            readers: Readers {
                images,
                bmap,
                releases,
            },
        }
    }

    /// The disk `device` resolves to, if it's safe to overwrite whatever
    /// the image: never the system disk, a non-removable one only with
    /// `force`.
    async fn guard_disk(&self, device: &Path, force: bool) -> Result<DiskInfo> {
        let info = self
            .disk
            .info(device)
            .await
            .change_context_lazy(|| Error::Disk(device.to_path_buf()))?;
        if info.is_system_disk {
            return Err(Report::new(Error::UnsafeTarget {
                path: info.path.clone(),
                reason: "this looks like the system disk",
            }));
        }
        if !info.is_removable && !force {
            return Err(Report::new(Error::UnsafeTarget {
                path: info.path.clone(),
                reason: "target is not marked removable; pass --force to override",
            }));
        }
        Ok(info)
    }
}

#[async_trait::async_trait]
impl FlashServiceInterface for FlashControllerImpl {
    async fn disk_images(
        &self,
        over: Option<&ContextOverride>,
        release: &str,
    ) -> Result<Vec<DiskImage>> {
        self.readers
            .releases
            .disk_images(over, release)
            .await
            .change_context_lazy(|| Error::DiskImages(release.to_owned()))
    }

    async fn inspect(&self, request: &FlashRequest) -> Result<ImageSummary> {
        let readers = self.readers.clone();
        let request = request.clone();
        let fetch = Fetch::unfollowed();
        tokio::task::spawn_blocking(move || Ok(open_image(&readers, &request, &fetch)?.summary))
            .await
            .expect("image inspection panicked")
    }

    async fn preflight(&self, request: &FlashRequest) -> Result<DiskInfo> {
        let info = self.guard_disk(&request.device, request.force).await?;
        check_fits(&self.inspect(request).await?, &info)?;
        Ok(info)
    }

    async fn flash(&self, request: &FlashRequest, ctx: &OperationContext) -> Result<FlashOutcome> {
        let info = self.guard_disk(&request.device, request.force).await?;

        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("flashing");

        // The copy is one long blocking call into `bmap_parser`: it runs on
        // a blocking-pool thread, while this one reports how much of the
        // image it has copied, from the count the image's stream shares.
        let progress = Progress::default();
        if let ImageOrigin::Artifact(artifact) = &request.image {
            progress.download.store(artifact.size, Ordering::Relaxed);
        }
        let fetch = Fetch {
            runtime: Handle::current(),
            cancel: ctx.cancel.clone(),
            received: progress.received.clone(),
        };
        let mut copy = tokio::task::spawn_blocking({
            let readers = self.readers.clone();
            let request = request.clone();
            let progress = progress.clone();
            move || run_flash(&readers, &request, &info, &progress, &fetch)
        });
        let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stopping = false;
        let outcome = loop {
            tokio::select! {
                outcome = &mut copy => break outcome.expect("flash worker panicked"),
                _ = ctx.cancel.cancelled(), if !stopping => {
                    stopping = true;
                    ctx.sink.log("stopping... (Ctrl-C again to quit at once)");
                }
                _ = tick.tick(), if !stopping => progress.report(&ctx.sink),
            }
        };
        let (outcome, device) = match outcome {
            Ok(done) => {
                progress.report(&ctx.sink);
                done
            }
            // The image's stream failing its reads is how a cancel stops
            // the copy: say so rather than the read error it shows up as.
            Err(report) if ctx.cancel.is_cancelled() => {
                return Err(report.change_context(Error::Cancelled))
            }
            Err(report) => return Err(report),
        };

        // What the copy wrote may still sit in the kernel's buffers: done
        // means on the disk, ready to be unplugged.
        ctx.sink.phase("syncing");
        let path = request.device.clone();
        tokio::task::spawn_blocking(move || device.sync_all())
            .await
            .expect("sync worker panicked")
            .change_context(Error::Sync(path))?;
        Ok(outcome)
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How much of the image the copy has copied, out of how much it will (0
/// while unknown), and for a release artifact how much of it was
/// downloaded, out of its size; shared between the copy's thread and the
/// reporting one.
#[derive(Clone, Default)]
struct Progress {
    copied: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
    /// The artifact's size; 0 for a local image.
    download: Arc<AtomicU64>,
}

impl Progress {
    fn report(&self, sink: &ProgressSink) {
        let download = self.download.load(Ordering::Relaxed);
        if download > 0 {
            sink.transfer(self.received.load(Ordering::Relaxed), download);
        }
        let done = self.copied.load(Ordering::Relaxed);
        match self.total.load(Ordering::Relaxed) {
            // A compressed image without a bmap: no size to go by but the
            // decompressed bytes themselves.
            0 => sink.log(format!("{} written", human_size(done))),
            total => sink.progress_bytes(done, total),
        }
    }
}

/// An image opened for copying, its `.bmap` parsed, and what they add up
/// to.
struct OpenedImage {
    source: SourceImage,
    map: Option<BlockMap>,
    summary: ImageSummary,
}

fn open_image(readers: &Readers, request: &FlashRequest, fetch: &Fetch) -> Result<OpenedImage> {
    let mut source = match &request.image {
        ImageOrigin::File(path) => readers.images.open(path),
        ImageOrigin::Artifact(artifact) => {
            let file = RemoteFile::new(
                readers.releases.clone(),
                artifact.clone(),
                fetch.runtime.clone(),
                fetch.cancel.clone(),
                fetch.received.clone(),
            );
            let len = file.len();
            readers.images.open_file(Box::new(file), len)
        }
    }
    .change_context_lazy(|| Error::OpenImage(request.image.to_string()))?;
    let bmap = &readers.bmap;

    let bmap_xml = match &request.bmap {
        BmapSource::None => None,
        BmapSource::File(path) => Some((read_bmap(path)?, BmapOrigin::File(path.clone()))),
        BmapSource::Auto => match source.bundled_bmap.take() {
            Some(xml) => Some((xml, BmapOrigin::Bundled)),
            // A release's disk image comes with its own, bundled.
            None => match &request.image {
                ImageOrigin::File(path) => sibling_bmap(path),
                ImageOrigin::Artifact(_) => None,
            }
            .map(|path| Ok::<_, Report<Error>>((read_bmap(&path)?, BmapOrigin::File(path))))
            .transpose()?,
        },
    };
    let (map, origin) = match bmap_xml {
        Some((xml, origin)) => (
            Some(bmap.parse(&xml).change_context(Error::Bmap)?),
            Some(origin),
        ),
        None => (None, None),
    };

    let summary = ImageSummary {
        bundle: source.bundle,
        compression: source.compression,
        image_size: map.as_ref().map(BlockMap::image_size).or(source.size),
        bmap: map.as_ref().zip(origin).map(|(map, origin)| BmapSummary {
            origin,
            mapped_size: map.total_mapped_size(),
            ranges: map.ranges(),
            checksum: map.checksum(),
        }),
    };
    Ok(OpenedImage {
        source,
        map,
        summary,
    })
}

/// The image must fit the disk whole, not just its mapped ranges: its
/// partition table describes the whole of it (a GPT's backup header sits
/// in its very last blocks), as `bmaptool` has it too.
fn check_fits(summary: &ImageSummary, disk: &DiskInfo) -> Result<()> {
    match summary.image_size {
        Some(image) if image > disk.size_bytes => Err(Report::new(Error::ImageTooLarge {
            path: disk.path.clone(),
            image,
            disk: disk.size_bytes,
        })),
        _ => Ok(()),
    }
}

/// Copies the image to the disk `checked` describes; hands the disk back,
/// written but maybe not yet flushed.
fn run_flash(
    readers: &Readers,
    request: &FlashRequest,
    checked: &DiskInfo,
    progress: &Progress,
    fetch: &Fetch,
) -> Result<(FlashOutcome, File)> {
    let OpenedImage {
        source,
        map,
        summary,
    } = open_image(readers, request, fetch)?;
    let bmap = &readers.bmap;
    check_fits(&summary, checked)?;

    let mut device = open_checked_device(&request.device, checked)?;

    progress
        .total
        .store(summary.bytes_to_write().unwrap_or(0), Ordering::Relaxed);
    let mut image = source.stream;
    image.follow(progress.copied.clone(), fetch.cancel.clone());

    let outcome = if let Some(map) = map {
        bmap.copy_with_bmap(&mut image, &mut device, &map)
            .change_context(Error::Bmap)?;
        FlashOutcome {
            bytes_written: map.total_mapped_size(),
            used_bmap: true,
        }
    } else {
        bmap.copy_without_bmap(&mut image, &mut device)
            .change_context(Error::Bmap)?;
        FlashOutcome {
            bytes_written: image.position(),
            used_bmap: false,
        }
    };
    Ok((outcome, device))
}

fn read_bmap(path: &Path) -> Result<String> {
    fs::read_to_string(path).change_context_lazy(|| Error::OpenBmap(path.to_path_buf()))
}

/// Compression extensions a plain image's bmap is named without, the way
/// Yocto names them: `x.wic.bz2` comes with `x.wic.bmap`.
const COMPRESSION_EXTENSIONS: &[&str] = &["bz2", "gz", "zst"];

/// The `.bmap` next to a plain image, as `bmaptool` looks it up:
/// `<image>.bmap`, else the image's name without its compression extension
/// plus `.bmap`.
fn sibling_bmap(image: &Path) -> Option<PathBuf> {
    let with_bmap = |path: &Path| {
        let mut name = path.as_os_str().to_owned();
        name.push(".bmap");
        PathBuf::from(name)
    };
    let mut candidates = vec![with_bmap(image)];
    if image
        .extension()
        .is_some_and(|ext| COMPRESSION_EXTENSIONS.iter().any(|c| ext == *c))
    {
        candidates.push(with_bmap(&image.with_extension("")));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Open the disk the guard checked, and only if `device` still resolves to
/// it: the check was made on `checked.path`, the canonical node, so a
/// `device` resolving anywhere else (a symlink retargeted since, a disk
/// service describing another node) is refused rather than written.
fn open_checked_device(device: &Path, checked: &DiskInfo) -> Result<File> {
    let resolved =
        fs::canonicalize(device).change_context_lazy(|| Error::OpenDevice(device.to_path_buf()))?;
    if resolved != checked.path {
        return Err(Report::new(Error::TargetMismatch {
            device: device.to_path_buf(),
            resolved,
            checked: checked.path.clone(),
        }));
    }
    OpenOptions::new()
        .write(true)
        .open(&checked.path)
        .change_context_lazy(|| Error::OpenDevice(checked.path.clone()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static FIXTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway directory under the OS temp dir, removed on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let id = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "remora-flash-controller-test-{}-{}",
                std::process::id(),
                id
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn touch(&self, name: &str) -> PathBuf {
            let path = self.dir.join(name);
            fs::write(&path, b"").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn sibling_bmap_finds_the_image_name_plus_bmap() {
        let dir = TempDir::new();
        let image = dir.touch("disk.wic.bz2");
        let bmap = dir.touch("disk.wic.bz2.bmap");
        dir.touch("disk.wic.bmap");

        assert_eq!(sibling_bmap(&image), Some(bmap));
    }

    #[test]
    fn sibling_bmap_falls_back_to_the_name_without_compression() {
        let dir = TempDir::new();
        let image = dir.touch("disk.wic.bz2");
        let bmap = dir.touch("disk.wic.bmap");

        assert_eq!(sibling_bmap(&image), Some(bmap));
    }

    #[test]
    fn sibling_bmap_strips_only_a_compression_extension() {
        let dir = TempDir::new();
        let image = dir.touch("disk.wic");
        dir.touch("disk.bmap");

        assert_eq!(sibling_bmap(&image), None);
    }
}
