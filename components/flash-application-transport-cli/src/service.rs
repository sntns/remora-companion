use std::path::PathBuf;

use error_stack::{Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use remora_disk::application::DiskService;
use remora_flash::{
    application::FlashService,
    model::{
        BmapOrigin, BmapSource, DiskImage, FlashRequest, ImageOrigin, ImageSummary,
        ReleaseArtifact, DEFAULT_IMAGE_TYPE,
    },
};
use remora_format::human_size;

use super::error::{Error, Result};

#[derive(Debug, clap::Args)]
pub struct Command {
    /// Source disk image: a `.bmaptar` bundle (the image and its .bmap in
    /// one tar, as meta-remora builds them), or a plain image, raw or
    /// compressed (bzip2, gzip, zstd).
    #[arg(long, required_unless_present = "release", conflicts_with = "release")]
    image: Option<PathBuf>,

    /// Flash an image of this release instead, downloaded while it's
    /// written (as the selected context): one of its artifacts tagged
    /// `type:<--type>`, picked by --board, or asked for when there are
    /// several.
    #[arg(long)]
    #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
    release: Option<String>,

    /// With --release: which of its images, by their `type:` tag:
    /// `diskimage` (the default), the device's own disk, or `installer`, a
    /// USB installer that flashes it onto the device it boots.
    // Defaulted at lookup, not by clap: the help says what it is, and a
    // local --image has no type to show.
    #[arg(long = "type", value_name = "TYPE", requires = "release")]
    image_type: Option<String>,

    /// With --release: the board to flash the image of (its artifact's
    /// `board:` tag).
    #[arg(long, requires = "release")]
    board: Option<String>,

    /// With --release: the artifact to flash, by file name, when the board
    /// isn't enough to tell.
    #[arg(long, requires = "release")]
    artifact: Option<String>,

    /// .bmap file describing the image's mapped block ranges. Defaults to
    /// the one a `.bmaptar` bundle carries; for a plain image, to
    /// `<image>.bmap`, else to the image's name without its compression
    /// extension plus `.bmap` (`x.wic.bz2` -> `x.wic.bmap`), whichever
    /// exists (same convention as bmaptool); pass --no-bmap to skip this.
    #[arg(long)]
    bmap: Option<PathBuf>,

    /// Copy the whole image verbatim (no sparse skip, no checksum
    /// verification), ignoring any .bmap, bundled or next to the image.
    #[arg(long, conflicts_with = "bmap")]
    no_bmap: bool,

    /// Destination device path, e.g. /dev/sdb. Its *whole disk* is
    /// overwritten — not a single partition.
    #[arg(long)]
    #[arg(add = remora_completion::values(remora_completion::Kind::Disk))]
    device: PathBuf,

    /// Allow flashing a disk that isn't marked removable. Never overrides
    /// the system-disk refusal.
    #[arg(long)]
    force: bool,

    /// Accepted for scripts written when flash asked for confirmation; it
    /// no longer does.
    #[arg(long, hide = true)]
    yes: bool,
}

pub async fn run(
    command: Command,
    disk: &DiskService,
    flash: &FlashService,
    contexts: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let Command {
        image,
        release,
        image_type,
        board,
        artifact,
        bmap,
        no_bmap,
        device,
        force,
        yes: _,
    } = command;

    let bmap = match bmap {
        Some(path) => BmapSource::File(path),
        None if no_bmap => BmapSource::None,
        None => BmapSource::Auto,
    };

    let image = match (image, release) {
        (Some(path), _) => ImageOrigin::File(path),
        (None, Some(release)) => {
            let image_type = image_type.as_deref().unwrap_or(DEFAULT_IMAGE_TYPE);
            let images = flash
                .disk_images(over, &release, image_type)
                .await
                .change_context(Error::Flash)?;
            let chosen = choose(
                &release,
                image_type,
                images,
                board.as_deref(),
                artifact.as_deref(),
            )?;
            ImageOrigin::Artifact(ReleaseArtifact {
                over: over.cloned(),
                release,
                file_name: chosen.file_name,
                size: chosen.size,
                tag_condition: chosen.tag_condition,
            })
        }
        (None, None) => unreachable!("clap requires --image or --release"),
    };
    let request = FlashRequest {
        image,
        bmap,
        device,
        force,
    };

    // Shown before the guard runs, so a refused flash still shows what it
    // was: the image as it reads, the target as the disk it resolves to,
    // not the name it was given.
    let summary = flash.inspect(&request).await.change_context(Error::Flash)?;
    // Where a download comes from: the context it runs as.
    let remote = match &request.image {
        ImageOrigin::Artifact(_) => Some(
            contexts
                .resolve(over)
                .await
                .change_context(Error::Context)?,
        ),
        ImageOrigin::File(_) => None,
    };
    remora_tui::note(
        "Image",
        image_lines(&request.image, remote.as_ref(), &summary),
    );
    let info = disk
        .info(&request.device)
        .await
        .change_context(Error::Disk)?;
    remora_tui::note("Target", disk_line(&info));

    // The guard (removable / not-the-system-disk / big enough) lives in
    // FlashServiceInterface::preflight, which `flash` runs again on its own
    // lookup and cannot be bypassed from here: run here, it refuses before
    // anything starts. No confirmation on top of it: the target and the
    // image are on screen above.
    flash
        .preflight(&request)
        .await
        .change_context(Error::Flash)?;

    let (sink, stream) = remora_progress::channel();
    let follow = remora_tui::follow(stream);
    let ctx = remora_progress::OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
    let outcome = flash.flash(&request, &ctx).await;
    drop(ctx);
    follow.finish(&outcome).await;
    let outcome = outcome.change_context(Error::Flash)?;
    remora_tui::success(format!(
        "Wrote {} to {} {}",
        human_size(outcome.bytes_written),
        remora_tui::accent(info.path.display()),
        remora_tui::dim(if outcome.used_bmap {
            "(bmap-verified)"
        } else {
            "(full copy, no bmap)"
        })
    ));
    Ok(())
}

/// The image of `release` to flash, of type `image_type`: the `artifact`
/// named, else the only one for `board` (or the only one at all), else the
/// operator's pick.
fn choose(
    release: &str,
    image_type: &str,
    images: Vec<DiskImage>,
    board: Option<&str>,
    artifact: Option<&str>,
) -> Result<DiskImage> {
    let boards = |images: &[DiskImage]| {
        let mut all: Vec<_> = images
            .iter()
            .flat_map(|image| image.boards.clone())
            .collect();
        all.sort();
        all.dedup();
        if all.is_empty() {
            "none tagged".to_owned()
        } else {
            all.join(", ")
        }
    };
    if images.is_empty() {
        return Err(Report::new(Error::NoDiskImage(
            release.to_owned(),
            image_type.to_owned(),
        ))
        .attach(format!(
            "its {image_type} images are the artifacts tagged type:{image_type}"
        )));
    }
    let available = boards(&images);
    let mut candidates: Vec<_> = images
        .into_iter()
        .filter(|image| artifact.is_none_or(|name| image.file_name == name))
        .filter(|image| board.is_none_or(|board| image.boards.iter().any(|b| b == board)))
        .collect();
    match candidates.len() {
        0 => Err(Report::new(Error::NoDiskImage(
            release.to_owned(),
            image_type.to_owned(),
        ))
        .attach(format!(
            "boards it has {image_type} images for: {available}"
        ))),
        1 => Ok(candidates.remove(0)),
        _ if !remora_tui::interactive() => Err(Report::new(Error::SeveralDiskImages(
            release.to_owned(),
            image_type.to_owned(),
        ))
        .attach(format!("pass --board, one of: {}", boards(&candidates)))),
        _ => {
            let mut select = remora_tui::select(format!(
                "{image_type} image of {} to flash",
                remora_tui::accent(release)
            ));
            for (i, image) in candidates.iter().enumerate() {
                let label = match image.boards.as_slice() {
                    [] => image.file_name.clone(),
                    boards => boards.join(", "),
                };
                select = select.item(
                    i,
                    label,
                    format!("{}  {}", image.file_name, human_size(image.size)),
                );
            }
            let picked = select.interact().change_context(Error::Choose)?;
            Ok(candidates.remove(picked))
        }
    }
}

/// The Image note: the image's name, where a release's is downloaded
/// from, then one line each for its format, sizes and `.bmap`.
fn image_lines(
    origin: &ImageOrigin,
    remote: Option<&ResolvedContext>,
    summary: &ImageSummary,
) -> String {
    let mut lines = Vec::new();
    match origin {
        ImageOrigin::File(path) => lines.push(remora_tui::accent(
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy(),
        )),
        ImageOrigin::Artifact(artifact) => {
            lines.push(remora_tui::accent(&artifact.file_name));
            let mut from = format!(
                "{} {} from release {}",
                remora_tui::dim("download:"),
                human_size(artifact.size),
                remora_tui::accent(&artifact.release)
            );
            if let Some(remote) = remote {
                let mut context = format!("context {}", remote.context.name);
                if let Some(role) = &remote.role {
                    context.push_str(&format!(" as {}", role.display_name()));
                }
                from.push_str(&format!(", {}", remora_tui::dim(context)));
            }
            lines.push(from);
            if !artifact.tag_condition.is_empty() {
                lines.push(format!(
                    "{} {}",
                    remora_tui::dim("tags:"),
                    artifact.tag_condition
                ));
            }
        }
    }
    let format = match (summary.bundle, summary.compression) {
        (true, Some(compression)) => format!("bmaptar bundle, {compression}-compressed"),
        (true, None) => "bmaptar bundle, raw".to_owned(),
        (false, Some(compression)) => format!("{compression}-compressed"),
        (false, None) => "raw".to_owned(),
    };
    let size = match summary.image_size {
        Some(size) => human_size(size),
        None => "unknown until decompressed".to_owned(),
    };
    let to_write = match (summary.bytes_to_write(), summary.image_size) {
        (Some(bytes), Some(size)) if summary.bmap.is_some() && size > 0 => format!(
            "{} ({:.1}%)",
            human_size(bytes),
            bytes as f64 * 100.0 / size as f64
        ),
        (Some(bytes), _) => human_size(bytes),
        (None, _) => "all of it".to_owned(),
    };
    let bmap = match &summary.bmap {
        Some(bmap) => format!(
            "{}, {} ranges, {}-verified",
            match &bmap.origin {
                BmapOrigin::Bundled => "bundled".to_owned(),
                BmapOrigin::File(path) => path.display().to_string(),
            },
            bmap.ranges,
            bmap.checksum
        ),
        None => "none: the whole image is copied as is, unverified".to_owned(),
    };
    let label = remora_tui::dim;
    lines.push(format!("{} {format}", label("format:")));
    lines.push(format!(
        "{} {size}  {} {to_write}",
        label("size:"),
        label("to write:")
    ));
    lines.push(format!("{} {bmap}", label("bmap:")));
    lines.join("\n")
}

fn disk_line(info: &remora_disk::model::DiskInfo) -> String {
    let yes_no = |on: bool| if on { "yes" } else { "no" };
    let label = remora_tui::dim;
    format!(
        "{}  {}\n{} {}  {} {}  {} {}",
        remora_tui::accent(info.path.display()),
        info.model.as_deref().unwrap_or("-"),
        label("size:"),
        human_size(info.size_bytes),
        label("removable:"),
        yes_no(info.is_removable),
        label("system:"),
        yes_no(info.is_system_disk),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_bmap_conflicts_with_bmap_on_the_command_line() {
        use clap::Parser;

        #[derive(Debug, clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            command: Command,
        }

        let err = TestCli::try_parse_from([
            "remora-etcher",
            "--image",
            "disk.img",
            "--device",
            "/dev/sdx",
            "--bmap",
            "disk.img.bmap",
            "--no-bmap",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[derive(Debug, clap::Parser)]
    struct TypeCli {
        #[command(flatten)]
        command: Command,
    }

    #[test]
    fn a_release_image_is_a_disk_image_unless_another_type_is_asked_for() {
        use clap::Parser;

        let parse = |extra: &[&str]| {
            let mut args = vec!["remora-etcher", "--release", "v1", "--device", "/dev/sdx"];
            args.extend(extra);
            TypeCli::try_parse_from(args).unwrap().command.image_type
        };
        assert_eq!(
            parse(&[]),
            None,
            "defaults to {DEFAULT_IMAGE_TYPE} at lookup"
        );
        assert_eq!(
            parse(&["--type", "installer"]).as_deref(),
            Some("installer")
        );
    }
}
