use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_convert::application::{self as convert, ConvertService};
use remora_image::{
    application::ImageService,
    model::{DumpRequest, FillRequest, PartitionRole, PartitionSelector},
};
use remora_installer::{
    application::{Error, InstallerServiceInterface, Result},
    model::{PackOutcome, PackRequest, UnpackOutcome, UnpackRequest},
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
        let scratch = scratch_next_to(&request.output)?;

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

    async fn unpack(
        &self,
        request: &UnpackRequest,
        ctx: &OperationContext,
    ) -> Result<UnpackOutcome> {
        let scratch = scratch_next_to(&request.output)?;

        let raw = scratch.join("installer.wic");
        self.convert
            .to_raw(&request.installer, &raw, &scoped(ctx, "installer"))
            .await
            .change_context_lazy(|| Error::DecodeInstaller(request.installer.clone()))?;

        ctx.sink.phase("extracting the image");
        let table = self
            .image
            .inspect(&raw)
            .await
            .change_context(Error::Extract)?;
        let partition_index = table
            .select_role(PartitionRole::Installer)
            .change_context(Error::Extract)?
            .index;
        self.image
            .dump_partition(
                &DumpRequest {
                    image: raw.clone(),
                    partition: PartitionSelector::Index(partition_index),
                    output: request.output.clone(),
                },
                ctx,
            )
            .await
            .change_context(Error::Extract)?;

        // The partition, up to its 1 MiB boundary, past the tar's end:
        // only the tar is the image's bundle.
        let trimmed = (|| {
            if !starts_with_tar(&request.output)? {
                return Err(Report::new(Error::NoPayload(request.installer.clone())));
            }
            let len = tar_len(&request.output)?;
            OpenOptions::new()
                .write(true)
                .open(&request.output)
                .and_then(|file| file.set_len(len))
                .change_context_lazy(|| Error::Write(request.output.clone()))?;
            Ok(len)
        })();
        let payload_bytes = match trimmed {
            Ok(len) => len,
            Err(report) => {
                let _ = std::fs::remove_file(&request.output);
                return Err(report);
            }
        };

        Ok(UnpackOutcome {
            payload_bytes,
            partition_index,
        })
    }
}

/// A scratch directory next to `output`, not in the system's temp dir: a
/// raw installer is sparse, but its payload is real data, often more than
/// a tmpfs holds.
fn scratch_next_to(output: &Path) -> Result<ScratchDir> {
    let dir = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    ScratchDir::new_in(&dir, ".remora-installer-")
        .change_context_lazy(|| Error::Scratch(output.to_path_buf()))
}

/// `ctx`, its phases named after `scope`: `convert`'s own ("decoding",
/// "encoding") say which input they're about.
fn scoped(ctx: &OperationContext, scope: &str) -> OperationContext {
    OperationContext::new(ctx.sink.scoped(scope), ctx.cancel.clone())
}

/// Where a tar header's magic sits: "ustar", in POSIX and GNU tars alike.
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

const TAR_BLOCK: u64 = 512;
/// What a tar is written by: its end padded to a whole record, 20 blocks.
const TAR_RECORD: u64 = 20 * TAR_BLOCK;

/// The length of the tar at the start of `path`, its end-of-archive and
/// last record's padding included: whatever follows (a partition's zero
/// tail) is not part of it. Walks its headers, each followed by its
/// member's blocks; the first all-zero block starts the end-of-archive
/// marker, two blocks long.
fn tar_len(path: &Path) -> Result<u64> {
    let read = || Error::Read(path.to_path_buf());
    let mut file = File::open(path).change_context_lazy(read)?;
    let file_len = file.metadata().change_context_lazy(read)?.len();
    let mut header = [0u8; TAR_BLOCK as usize];
    let mut offset = 0u64;
    loop {
        if offset + TAR_BLOCK > file_len {
            // No end-of-archive: the whole file is the tar.
            return Ok(file_len);
        }
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut header))
            .change_context_lazy(read)?;
        if header.iter().all(|&b| b == 0) {
            let end = (offset + 2 * TAR_BLOCK).div_ceil(TAR_RECORD) * TAR_RECORD;
            return Ok(end.min(file_len));
        }
        let size = tar_member_size(&header).ok_or_else(|| Report::new(read()))?;
        offset += TAR_BLOCK + size.div_ceil(TAR_BLOCK) * TAR_BLOCK;
    }
}

/// A tar header's member size: octal, or GNU's base-256 (high bit of its
/// first byte set) for members past 8 GiB.
fn tar_member_size(header: &[u8; TAR_BLOCK as usize]) -> Option<u64> {
    let field = &header[124..136];
    if field[0] & 0x80 != 0 {
        return field[1..]
            .iter()
            .try_fold(u64::from(field[0] & 0x7f), |acc, &b| {
                acc.checked_mul(256)?.checked_add(u64::from(b))
            });
    }
    let digits = std::str::from_utf8(field)
        .ok()?
        .trim_matches(|c: char| c == '\0' || c == ' ');
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, 8).ok()
}

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
