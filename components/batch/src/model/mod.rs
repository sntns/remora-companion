use std::path::PathBuf;

use remora_image::model::{CpDirRequest, InjectRequest, MkdirRequest};
use remora_squashfs::model::BuildOptions;

/// One step of a batch recipe. Mirrors each vertical's own service method
/// one-for-one (same field names/types) rather than inventing a parallel
/// shape -- a step is just "the arguments to one existing call", not a new
/// concept of its own.
///
/// Serializable so a caller (the CLI's `batch run --recipe <file.json>`, or
/// a future GUI saving/loading a recipe) can express an arbitrary sequence
/// without new Rust code per combination; a GUI would just as easily build
/// a `Vec<BatchStep>` directly in memory and never touch JSON at all.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum BatchStep {
    /// See `remora_convert::application::ConvertServiceInterface::to_raw`.
    ConvertToRaw { image: PathBuf, output: PathBuf },
    /// See `ConvertServiceInterface::from_raw`.
    ConvertFromRaw { raw_image: PathBuf, output: PathBuf },
    /// See `remora_identity::application::IdentityServiceInterface::create`.
    IdentityCreate {
        inputs: Vec<PathBuf>,
        image: PathBuf,
        hostname: Option<String>,
        machine_id: Option<String>,
    },
    /// See `remora_config::application::ConfigServiceInterface::upload`.
    ConfigUpload {
        image: PathBuf,
        source: PathBuf,
        dest_relative_path: String,
        slot: String,
        mode: u16,
    },
    /// See `remora_image::application::ImageServiceInterface::inject`.
    ImageInject(InjectRequest),
    /// See `ImageServiceInterface::mkdir`.
    ImageMkdir(MkdirRequest),
    /// See `ImageServiceInterface::cp_dir`.
    ImageCpDir(CpDirRequest),
    /// See `remora_squashfs::application::SquashfsServiceInterface::build`.
    SquashfsBuild {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        options: BuildOptions,
    },
    /// See `remora_factory::application::FactoryServiceInterface::provision`.
    ///
    /// Unlike every other step, this one is a network call against a
    /// specific platform environment with per-unit output (a new key and
    /// serial each time) -- a recipe containing it is not
    /// reproducible/replayable offline the way the rest of `batch` is.
    /// Included anyway on request; see the batch vertical's own doc
    /// comment for the tradeoff. Produces a standalone `remora-factory.yaml`
    /// at `output`, same as running `factory provision` directly -- pair it
    /// with a later `IdentityCreate` step (`inputs` including this file) to
    /// bundle it into an image.
    ///
    /// Exactly one of `device_name`/`serial_number_policy` must be set;
    /// `force` only with `device_name` (see `remora_factory::model::DeviceSerial`).
    FactoryProvision {
        #[serde(default)]
        device_name: Option<String>,
        #[serde(default)]
        serial_number_policy: Option<String>,
        /// The context to manufacture as; default: the batch's own
        /// (`--context`, else the current one).
        #[serde(default)]
        context: Option<String>,
        /// Escape hatch for a deployment that hasn't configured an
        /// access-url yet; see `FactoryServiceInterface::provision`'s doc
        /// comment. Normally omitted -- the platform's response supplies it.
        #[serde(default)]
        access_url: Option<String>,
        /// See `FactoryServiceInterface::provision`'s `force` doc comment.
        #[serde(default)]
        force: bool,
        output: PathBuf,
    },
}

impl BatchStep {
    /// Short, stable name for progress/error reporting (`"step 2/5:
    /// image-cp-dir"`) -- the same spelling `#[serde(tag = "step")]` uses on
    /// the wire, so a message matches what a recipe file actually says.
    pub fn kind(&self) -> &'static str {
        match self {
            BatchStep::ConvertToRaw { .. } => "convert-to-raw",
            BatchStep::ConvertFromRaw { .. } => "convert-from-raw",
            BatchStep::IdentityCreate { .. } => "identity-create",
            BatchStep::ConfigUpload { .. } => "config-upload",
            BatchStep::ImageInject(_) => "image-inject",
            BatchStep::ImageMkdir(_) => "image-mkdir",
            BatchStep::ImageCpDir(_) => "image-cp-dir",
            BatchStep::SquashfsBuild { .. } => "squashfs-build",
            BatchStep::FactoryProvision { .. } => "factory-provision",
        }
    }
}
