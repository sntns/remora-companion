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
use remora_disk::{application::DiskService, model::DiskInfo};
use remora_flash::{
    adapter::{bmap::BmapAdapterService, source::ImageSourceAdapterService},
    application::{Error, FlashServiceInterface, Result},
    model::{BmapSource, FlashOutcome, FlashRequest},
};
use remora_format::human_size;
use remora_progress::{OperationContext, ProgressSink};
use tokio_util::sync::CancellationToken;

/// The flash vertical's use case. The actual safety guard (`preflight`) lives
/// here, not in the adapter — so nothing, CLI or a future GUI, can reach the
/// raw copy without going through it first. It asks the disk vertical about
/// the target itself rather than trusting a description of it.
pub struct FlashControllerImpl {
    disk: DiskService,
    images: ImageSourceAdapterService,
    bmap: BmapAdapterService,
}

impl FlashControllerImpl {
    pub fn new(
        disk: DiskService,
        images: ImageSourceAdapterService,
        bmap: BmapAdapterService,
    ) -> Self {
        Self { disk, images, bmap }
    }
}

#[async_trait::async_trait]
impl FlashServiceInterface for FlashControllerImpl {
    async fn preflight(&self, device: &Path, force: bool) -> Result<DiskInfo> {
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

    async fn flash(&self, request: &FlashRequest, ctx: &OperationContext) -> Result<FlashOutcome> {
        let info = self.preflight(&request.device, request.force).await?;

        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("flashing");

        // The copy is one long blocking call into `bmap_parser`: it runs on
        // a blocking-pool thread, while this one reports how far into the
        // image it has read, from the position the image's stream shares.
        let progress = Progress::default();
        let mut copy = tokio::task::spawn_blocking({
            let images = self.images.clone();
            let bmap = self.bmap.clone();
            let request = request.clone();
            let progress = progress.clone();
            let cancel = ctx.cancel.clone();
            move || run_flash(&images, &bmap, &request, &info, &progress, cancel)
        });
        let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let outcome = loop {
            tokio::select! {
                outcome = &mut copy => break outcome.expect("flash worker panicked"),
                _ = tick.tick() => progress.report(&ctx.sink),
            }
        };
        match outcome {
            Ok(outcome) => {
                progress.report(&ctx.sink);
                Ok(outcome)
            }
            // The image's stream failing its reads is how a cancel stops
            // the copy: say so rather than the read error it shows up as.
            Err(report) if ctx.cancel.is_cancelled() => {
                Err(report.change_context(Error::Cancelled))
            }
            Err(report) => Err(report),
        }
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How far into the image the copy has read, out of how much (0 while
/// unknown), shared between the copy's thread and the reporting one.
#[derive(Clone, Default)]
struct Progress {
    position: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl Progress {
    fn report(&self, sink: &ProgressSink) {
        let done = self.position.load(Ordering::Relaxed);
        match self.total.load(Ordering::Relaxed) {
            // A compressed image without a bmap: no size to go by but the
            // decompressed bytes themselves.
            0 => sink.log(format!("{} written", human_size(done))),
            total => sink.progress(done, total),
        }
    }
}

fn run_flash(
    images: &ImageSourceAdapterService,
    bmap: &BmapAdapterService,
    request: &FlashRequest,
    checked: &DiskInfo,
    progress: &Progress,
    cancel: CancellationToken,
) -> Result<FlashOutcome> {
    let source = images
        .open(&request.image)
        .change_context_lazy(|| Error::OpenImage(request.image.clone()))?;

    let mut device = open_checked_device(&request.device, checked)?;

    let bmap_xml = match &request.bmap {
        BmapSource::None => None,
        BmapSource::File(path) => Some(read_bmap(path)?),
        BmapSource::Auto => match source.bundled_bmap {
            Some(xml) => Some(xml),
            None => sibling_bmap(&request.image)
                .map(|path| read_bmap(&path))
                .transpose()?,
        },
    };

    let map = bmap_xml
        .map(|xml| bmap.parse(&xml).change_context(Error::Bmap))
        .transpose()?;
    let total = map.as_ref().map(|map| map.image_size()).or(source.size);
    progress.total.store(total.unwrap_or(0), Ordering::Relaxed);
    let mut image = source.stream;
    image.follow(progress.position.clone(), cancel);

    if let Some(map) = map {
        let bytes_written = map.total_mapped_size();
        bmap.copy_with_bmap(&mut image, &mut device, &map)
            .change_context(Error::Bmap)?;
        Ok(FlashOutcome {
            bytes_written,
            used_bmap: true,
        })
    } else {
        bmap.copy_without_bmap(&mut image, &mut device)
            .change_context(Error::Bmap)?;
        Ok(FlashOutcome {
            bytes_written: image.position(),
            used_bmap: false,
        })
    }
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
