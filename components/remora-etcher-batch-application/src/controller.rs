use error_stack::{Report, ResultExt};
use remora_etcher_batch::{
    application::{BatchServiceInterface, Error, Result},
    model::BatchStep,
};
use remora_etcher_config::application::ConfigService;
use remora_etcher_convert::application::ConvertService;
use remora_etcher_factory::application::FactoryService;
use remora_etcher_identity::application::IdentityService;
use remora_etcher_image::application::ImageService;
use remora_etcher_progress::OperationContext;
use remora_etcher_squashfs::application::SquashfsService;

/// The batch vertical's use case: dispatches each `BatchStep` to whichever
/// other vertical's already-wired service handles it, reusing them exactly
/// as the CLI/a direct caller would -- this is orchestration only, no I/O
/// of its own. Only the verticals a `BatchStep` variant actually names are
/// injected; add a field (and wire it in `bootstrap::wire`) alongside any
/// new variant that needs one.
pub struct BatchControllerImpl {
    convert: ConvertService,
    identity: IdentityService,
    config: ConfigService,
    image: ImageService,
    squashfs: SquashfsService,
    factory: FactoryService,
}

impl BatchControllerImpl {
    pub fn new(
        convert: ConvertService,
        identity: IdentityService,
        config: ConfigService,
        image: ImageService,
        squashfs: SquashfsService,
        factory: FactoryService,
    ) -> Self {
        Self {
            convert,
            identity,
            config,
            image,
            squashfs,
            factory,
        }
    }
}

#[async_trait::async_trait]
impl BatchServiceInterface for BatchControllerImpl {
    async fn run(&self, steps: Vec<BatchStep>, ctx: &OperationContext) -> Result<()> {
        let total = steps.len();
        for (index, step) in steps.into_iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(Report::new(Error::Cancelled));
            }
            let kind = step.kind();
            ctx.sink
                .phase(format!("step {}/{total}: {kind}", index + 1));

            match step {
                BatchStep::ConvertToRaw { image, output } => self
                    .convert
                    .to_raw(&image, &output, ctx)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::ConvertFromRaw { raw_image, output } => self
                    .convert
                    .from_raw(&raw_image, &output, ctx)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::IdentityCreate {
                    inputs,
                    image,
                    hostname,
                    machine_id,
                } => self
                    .identity
                    .create(&inputs, hostname.as_deref(), machine_id.as_deref(), &image)
                    .await
                    .map(|_| ())
                    .change_context(Error::Step { index, kind })?,
                BatchStep::ConfigUpload {
                    image,
                    source,
                    dest_relative_path,
                    slot,
                    mode,
                } => self
                    .config
                    .upload(&image, &source, &dest_relative_path, &slot, mode)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::ImageInject(request) => self
                    .image
                    .inject(&request)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::ImageMkdir(request) => self
                    .image
                    .mkdir(&request)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::ImageCpDir(request) => self
                    .image
                    .cp_dir(&request, ctx)
                    .await
                    .change_context(Error::Step { index, kind })?,
                BatchStep::SquashfsBuild {
                    inputs,
                    output,
                    options,
                } => self
                    .squashfs
                    .build(&inputs, &output, &options, ctx)
                    .await
                    .map(|_| ())
                    .change_context(Error::Step { index, kind })?,
                BatchStep::FactoryProvision {
                    device_name,
                    gateway_url,
                    api_key,
                    access_url,
                    output,
                } => self
                    .factory
                    .provision(
                        &device_name,
                        &gateway_url,
                        &api_key,
                        access_url.as_deref(),
                        &output,
                        ctx,
                    )
                    .await
                    .change_context(Error::Step { index, kind })?,
            }
        }
        ctx.sink.phase("done");
        Ok(())
    }
}
