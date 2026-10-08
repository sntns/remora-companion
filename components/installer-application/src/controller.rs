use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_convert::application::{self as convert, ConvertService};
use remora_image::{
    application::ImageService,
    model::{FillRequest, PartitionRole, PartitionSelector},
};
use remora_installer::{
    application::{Error, InstallerServiceInterface, Result},
    model::{PackOutcome, PackRequest},
};
use remora_progress::OperationContext;
use remora_scratch::ScratchDir;

/// The installer vertical's use case: decode the installer to a raw image
/// (convert), put the image into its payload partition (image), encode
/// the result (convert) -- every byte handled by those verticals, this one
/// only deciding what goes where.
pub struct InstallerControllerImpl {
    convert: ConvertService,
    image: ImageService,
}

impl InstallerControllerImpl {
    pub fn new(convert: ConvertService, image: ImageService) -> Self {
        Self { convert, image }
    }

    /// `image` as a `.bmaptar`: itself when it is one, else bundled into
    /// `scratch` -- straight from a raw image, through a raw copy from any
    /// other format. Whether it had to be bundled, with the path.
    async fn bundled(
        &self,
        image: &Path,
        scratch: &ScratchDir,
        ctx: &OperationContext,
    ) -> Result<(PathBuf, bool)> {
        let is_tar = starts_with_tar(image)?;
        let named_bmaptar = image
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".bmaptar"));
        match (is_tar, named_bmaptar) {
            (true, _) => return Ok((image.to_path_buf(), false)),
            (false, true) => return Err(Report::new(Error::NotBmaptar(image.to_path_buf()))),
            (false, false) => {}
        }

        let bundle = scratch.join("image.wic.bmaptar");
        let bundle_error = || Error::BundleImage(image.to_path_buf());
        match self.convert.from_raw(image, &bundle, ctx).await {
            Ok(()) => {}
            // Compressed (or qcow2): through a raw copy, as `convert` reads
            // whatever its name says.
            Err(report) if matches!(report.current_context(), convert::Error::NotRaw(..)) => {
                let raw = scratch.join("image.wic");
                self.convert
                    .to_raw(image, &raw, ctx)
                    .await
                    .change_context_lazy(bundle_error)?;
                self.convert
                    .from_raw(&raw, &bundle, ctx)
                    .await
                    .change_context_lazy(bundle_error)?;
                let _ = std::fs::remove_file(&raw);
            }
            Err(report) => return Err(report.change_context(bundle_error())),
        }
        Ok((bundle, true))
    }
}

#[async_trait::async_trait]
impl InstallerServiceInterface for InstallerControllerImpl {
    async fn pack(&self, request: &PackRequest, ctx: &OperationContext) -> Result<PackOutcome> {
        let dir = match request.output.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        // Next to the output, not in the system's temp dir: the raw
        // installer is sparse but its payload is real data, often more
        // than a tmpfs holds.
        let scratch = ScratchDir::new_in(&dir, ".remora-installer-")
            .change_context_lazy(|| Error::Scratch(request.output.clone()))?;

        let raw = scratch.join("installer.wic");
        self.convert
            .to_raw(&request.installer, &raw, &scoped(ctx, "installer"))
            .await
            .change_context_lazy(|| Error::DecodeInstaller(request.installer.clone()))?;

        let (payload, bundled) = self
            .bundled(&request.image, &scratch, &scoped(ctx, "image"))
            .await?;

        ctx.sink.phase("embedding the image");
        let filled = self
            .image
            .fill_partition(
                &FillRequest {
                    image: raw.clone(),
                    source: payload,
                    partition: PartitionSelector::Role(PartitionRole::Installer),
                },
                ctx,
            )
            .await
            .change_context(Error::Embed)?;

        self.convert
            .from_raw(&raw, &request.output, &scoped(ctx, "installer"))
            .await
            .change_context_lazy(|| Error::Encode(request.output.clone()))?;

        Ok(PackOutcome {
            payload_bytes: filled.content_bytes,
            partition_index: filled.index,
            partition_bytes: filled.partition_bytes,
            bundled,
        })
    }
}

/// `ctx`, its phases named after `scope`: `convert`'s own ("decoding",
/// "encoding") say which input they're about.
fn scoped(ctx: &OperationContext, scope: &str) -> OperationContext {
    OperationContext::new(ctx.sink.scoped(scope), ctx.cancel.clone())
}

/// Where a tar header's magic sits: "ustar", in POSIX and GNU tars alike.
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

/// Whether `path` starts with a tar header: a `.bmaptar` bundle, whatever
/// its name.
fn starts_with_tar(path: &Path) -> Result<bool> {
    let mut header = Vec::with_capacity(512);
    File::open(path)
        .and_then(|file| file.take(512).read_to_end(&mut header))
        .change_context_lazy(|| Error::Read(path.to_path_buf()))?;
    Ok(header
        .get(TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + TAR_MAGIC.len())
        .is_some_and(|magic| magic == TAR_MAGIC))
}
