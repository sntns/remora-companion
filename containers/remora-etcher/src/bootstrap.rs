use std::sync::Arc;

use remora_batch::application::BatchService;
use remora_batch_application::BatchControllerImpl;
use remora_claim::{adapter::station::StationClientAdapterService, application::ClaimService};
use remora_claim_adapter_http::HttpStationClientImpl;
use remora_claim_application::ClaimControllerImpl;
use remora_config::application::ConfigService;
use remora_config_application::ConfigControllerImpl;
use remora_context::{
    adapter::{
        credentials::CredentialStoreAdapterService, platform::PlatformSessionAdapterService,
        store::ContextStoreAdapterService,
    },
    application::ContextService,
};
use remora_context_adapter_file::{default_root, FileContextStoreImpl, FileCredentialStoreImpl};
use remora_context_adapter_grpc::PlatformSessionAdapterImpl;
use remora_context_application::ContextControllerImpl;
use remora_convert::application::ConvertService;
use remora_convert_adapter_bmaptar::BmaptarAdapterImpl;
use remora_convert_adapter_bzip2::Bzip2AdapterImpl;
use remora_convert_adapter_gzip::GzipAdapterImpl;
use remora_convert_adapter_qcow2::Qcow2AdapterImpl;
use remora_convert_adapter_zstd::ZstdAdapterImpl;
use remora_convert_application::{ContainerFormats, ConvertControllerImpl};
use remora_device::{adapter::gateway::DeviceGatewayAdapterService, application::DeviceService};
use remora_device_adapter_grpc::DeviceGatewayAdapterImpl;
use remora_device_application::DeviceControllerImpl;
use remora_disk::{adapter::DiskAdapterService, application::DiskService};
use remora_disk_adapter_native::DiskAdapterImpl;
use remora_disk_application::DiskControllerImpl;
use remora_factory::{
    adapter::{
        credential::CredentialWriterAdapterService, key::DeviceKeyAdapterService,
        provisioning::FactoryProvisioningAdapterService,
    },
    application::FactoryService,
};
use remora_factory_adapter_grpc::FactoryGatewayAdapterImpl;
use remora_factory_adapter_local::{CredentialWriterAdapterImpl, DeviceKeyAdapterImpl};
use remora_factory_application::FactoryControllerImpl;
use remora_flash::{
    adapter::{
        bmap::BmapAdapterService, release::ReleaseArtifactAdapterService,
        source::ImageSourceAdapterService,
    },
    application::FlashService,
};
use remora_flash_adapter_bmap::BmapAdapterImpl;
use remora_flash_adapter_file::ImageSourceAdapterImpl;
use remora_flash_adapter_release::ReleaseArtifactAdapterImpl;
use remora_flash_application::FlashControllerImpl;
use remora_identity::{adapter::KeygenAdapterService, application::IdentityService};
use remora_identity_adapter_keygen::KeygenAdapterImpl;
use remora_identity_application::IdentityControllerImpl;
use remora_image::adapter::ext4::Ext4AdapterService;
use remora_image::{
    adapter::partition_table::PartitionTableAdapterService, application::ImageService,
};
use remora_image_adapter_ext4::Ext4AdapterImpl;
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_image_adapter_vfat::VfatAdapterImpl;
use remora_image_application::ImageControllerImpl;
use remora_ota::{
    adapter::{
        gateway::OtaGatewayAdapterService, source::ArtifactSourceAdapterService,
        target::ArtifactTargetAdapterService,
    },
    application::OtaService,
};
use remora_ota_adapter_file::{FileArtifactSourceImpl, FileArtifactTargetImpl};
use remora_ota_adapter_grpc::OtaGatewayAdapterImpl;
use remora_ota_application::OtaControllerImpl;
use remora_squashfs::{adapter::SquashfsAdapterService, application::SquashfsService};
use remora_squashfs_adapter_backhand::SquashfsAdapterImpl;
use remora_squashfs_application::SquashfsControllerImpl;
use remora_station::{
    adapter::{
        hooks::HookRunnerAdapterService, journal::JournalAdapterService,
        operator::OperatorAdapterService,
    },
    application::StationService,
};
use remora_station_adapter_hooks_process::ProcessHookRunnerImpl;
use remora_station_adapter_jsonl::JsonlJournalImpl;
use remora_station_adapter_operator_tui::TuiOperatorImpl;
use remora_station_application::StationControllerImpl;
use remora_update::{
    adapter::{feed::ReleaseFeedAdapterService, installer::InstallerAdapterService},
    application::UpdateService,
};
use remora_update_adapter_github::{DistInstallerImpl, GithubReleaseFeedImpl, RELEASES_REPO};
use remora_update_application::UpdateControllerImpl;

/// Every service the CLI can dispatch into, wired once at startup. All
/// fields deref all the way down to `dyn ...ServiceInterface` (`DiskService`
/// -> `Arc<Box<dyn DiskServiceInterface>>` -> `Box<dyn DiskServiceInterface>`
/// -> `dyn DiskServiceInterface`), so call sites just do `services.disk.list()`.
pub struct Services {
    pub context: ContextService,
    pub ota: OtaService,
    pub disk: DiskService,
    pub flash: FlashService,
    pub image: ImageService,
    pub squashfs: SquashfsService,
    pub identity: IdentityService,
    pub config: ConfigService,
    pub convert: ConvertService,
    pub batch: BatchService,
    pub factory: FactoryService,
    pub station: StationService,
    pub claim: ClaimService,
    pub update: UpdateService,
}

/// The composition root: construct each adapter, register it, pull it back
/// out to hand to the use case that depends on it, register that use case in
/// turn. Same recipe as remora-edge's `containers/remora-*d/src/daemon.rs`.
///
/// Async because `busybody`'s container API is, and because what it wires
/// is too: the ports and use cases are async (the context and factory
/// verticals make network calls), and the CLI dispatches into them on the
/// runtime `main()` builds; blocking work of meaningful size runs in
/// `spawn_blocking` inside the adapters.
pub async fn wire() -> Services {
    let container = busybody::ServiceContainerBuilder::new().build();

    container
        .set_type(DiskAdapterService::new(DiskAdapterImpl))
        .await;
    let disk_adapter = container
        .get_type::<DiskAdapterService>()
        .await
        .expect("DiskAdapterService was just registered");

    container
        .set_type(DiskService::new(DiskControllerImpl::new(disk_adapter)))
        .await;
    let disk = container
        .get_type::<DiskService>()
        .await
        .expect("DiskService was just registered");

    container
        .set_type(ImageSourceAdapterService::new(ImageSourceAdapterImpl))
        .await;
    let image_source = container
        .get_type::<ImageSourceAdapterService>()
        .await
        .expect("ImageSourceAdapterService was just registered");

    container
        .set_type(BmapAdapterService::new(BmapAdapterImpl))
        .await;
    let bmap_adapter = container
        .get_type::<BmapAdapterService>()
        .await
        .expect("BmapAdapterService was just registered");

    container
        .set_type(PartitionTableAdapterService::new(PartitionTableAdapterImpl))
        .await;
    let partition_table = container
        .get_type::<PartitionTableAdapterService>()
        .await
        .expect("PartitionTableAdapterService was just registered");

    container
        .set_type(remora_fs_walk::FsWalkAdapterService::new(
            remora_fs_walk::FsWalkAdapterImpl,
        ))
        .await;
    let fs_walk = container
        .get_type::<remora_fs_walk::FsWalkAdapterService>()
        .await
        .expect("FsWalkAdapterService was just registered");

    // ext4_fs/vfat_fs are consumed by exactly one constructor right below,
    // so there's no ambiguity to disambiguate via the container — skip the
    // set_type/get_type round-trip for these two (busybody keys purely on
    // TypeId, and `Arc<dyn PartitionFilesystem>` would collide between them
    // if both went through it under the same type).
    let ext4_fs: Arc<dyn remora_image::adapter::partition_fs::PartitionFilesystem> =
        Arc::new(Ext4AdapterImpl);
    let vfat_fs: Arc<dyn remora_image::adapter::partition_fs::PartitionFilesystem> =
        Arc::new(VfatAdapterImpl);

    // The image use case injects Ext4Adapter directly too (not through
    // PartitionFilesystem) for its standalone ext4 image operations — see
    // remora-image's Ext4Adapter doc comment.
    container
        .set_type(Ext4AdapterService::new(Ext4AdapterImpl))
        .await;
    let ext4_adapter = container
        .get_type::<Ext4AdapterService>()
        .await
        .expect("Ext4AdapterService was just registered");

    container
        .set_type(ImageService::new(ImageControllerImpl::new(
            partition_table,
            ext4_fs,
            vfat_fs,
            fs_walk.clone(),
            ext4_adapter,
        )))
        .await;
    let image = container
        .get_type::<ImageService>()
        .await
        .expect("ImageService was just registered");

    container
        .set_type(SquashfsAdapterService::new(SquashfsAdapterImpl))
        .await;
    let squashfs_adapter = container
        .get_type::<SquashfsAdapterService>()
        .await
        .expect("SquashfsAdapterService was just registered");

    container
        .set_type(SquashfsService::new(SquashfsControllerImpl::new(
            fs_walk,
            squashfs_adapter,
        )))
        .await;
    let squashfs = container
        .get_type::<SquashfsService>()
        .await
        .expect("SquashfsService was just registered");

    container
        .set_type(KeygenAdapterService::new(KeygenAdapterImpl))
        .await;
    let keygen_adapter = container
        .get_type::<KeygenAdapterService>()
        .await
        .expect("KeygenAdapterService was just registered");

    container
        .set_type(IdentityService::new(IdentityControllerImpl::new(
            keygen_adapter,
            squashfs.clone(),
            image.clone(),
        )))
        .await;
    let identity = container
        .get_type::<IdentityService>()
        .await
        .expect("IdentityService was just registered");

    let fs_walk_for_config = container
        .get_type::<remora_fs_walk::FsWalkAdapterService>()
        .await
        .expect("FsWalkAdapterService was registered earlier");

    container
        .set_type(ConfigService::new(ConfigControllerImpl::new(
            fs_walk_for_config,
            image.clone(),
        )))
        .await;
    let config = container
        .get_type::<ConfigService>()
        .await
        .expect("ConfigService was just registered");

    // The convert vertical injects its format adapters directly (like the
    // image vertical's ext4_fs/vfat_fs above), not through the busybody
    // container: ContainerFormatAdapter is implemented by several distinct
    // concrete types at once, which would collide under the same wrapper
    // TypeId if registered there.
    container
        .set_type(ConvertService::new(ConvertControllerImpl::new(
            ContainerFormats {
                qcow2: Arc::new(Qcow2AdapterImpl),
                gzip: Arc::new(GzipAdapterImpl),
                zstd: Arc::new(ZstdAdapterImpl),
                bzip2: Arc::new(Bzip2AdapterImpl),
                bmaptar: Arc::new(BmaptarAdapterImpl),
            },
        )))
        .await;
    let convert = container
        .get_type::<ConvertService>()
        .await
        .expect("ConvertService was just registered");

    // The same contexts as rmra's, in the same place: one login serves
    // both binaries.
    let root = default_root();
    container
        .set_type(ContextStoreAdapterService::new(FileContextStoreImpl::new(
            &root,
        )))
        .await;
    let context_store = container
        .get_type::<ContextStoreAdapterService>()
        .await
        .expect("ContextStoreAdapterService was just registered");

    container
        .set_type(CredentialStoreAdapterService::new(
            FileCredentialStoreImpl::new(&root),
        ))
        .await;
    let credential_store = container
        .get_type::<CredentialStoreAdapterService>()
        .await
        .expect("CredentialStoreAdapterService was just registered");

    container
        .set_type(PlatformSessionAdapterService::new(
            PlatformSessionAdapterImpl,
        ))
        .await;
    let platform_session = container
        .get_type::<PlatformSessionAdapterService>()
        .await
        .expect("PlatformSessionAdapterService was just registered");

    container
        .set_type(ContextService::new(ContextControllerImpl::new(
            context_store,
            credential_store,
            platform_session,
        )))
        .await;
    let context = container
        .get_type::<ContextService>()
        .await
        .expect("ContextService was just registered");

    // Flash after context: a release's disk image is downloaded through
    // the ota vertical, as the selected context. OTA's own deployments pick
    // devices through DeviceService, hence device first.
    container
        .set_type(DeviceGatewayAdapterService::new(DeviceGatewayAdapterImpl))
        .await;
    let device_gateway = container
        .get_type::<DeviceGatewayAdapterService>()
        .await
        .expect("DeviceGatewayAdapterService was just registered");

    container
        .set_type(DeviceService::new(DeviceControllerImpl::new(
            context.clone(),
            device_gateway,
        )))
        .await;
    let device = container
        .get_type::<DeviceService>()
        .await
        .expect("DeviceService was just registered");

    container
        .set_type(OtaGatewayAdapterService::new(OtaGatewayAdapterImpl))
        .await;
    let ota_gateway = container
        .get_type::<OtaGatewayAdapterService>()
        .await
        .expect("OtaGatewayAdapterService was just registered");

    container
        .set_type(ArtifactSourceAdapterService::new(FileArtifactSourceImpl))
        .await;
    let artifacts = container
        .get_type::<ArtifactSourceAdapterService>()
        .await
        .expect("ArtifactSourceAdapterService was just registered");

    container
        .set_type(ArtifactTargetAdapterService::new(FileArtifactTargetImpl))
        .await;
    let targets = container
        .get_type::<ArtifactTargetAdapterService>()
        .await
        .expect("ArtifactTargetAdapterService was just registered");

    container
        .set_type(OtaService::new(OtaControllerImpl::new(
            context.clone(),
            device,
            ota_gateway,
            artifacts,
            targets,
        )))
        .await;
    let ota = container
        .get_type::<OtaService>()
        .await
        .expect("OtaService was just registered");

    container
        .set_type(ReleaseArtifactAdapterService::new(
            ReleaseArtifactAdapterImpl::new(ota.clone()),
        ))
        .await;
    let releases = container
        .get_type::<ReleaseArtifactAdapterService>()
        .await
        .expect("ReleaseArtifactAdapterService was just registered");

    container
        .set_type(FlashService::new(FlashControllerImpl::new(
            disk.clone(),
            image_source,
            bmap_adapter,
            releases,
        )))
        .await;
    let flash = container
        .get_type::<FlashService>()
        .await
        .expect("FlashService was just registered");

    // Manufactures as the selected context, over the gateway's gRPC API;
    // the device key and remora-factory.yaml stay local.
    container
        .set_type(DeviceKeyAdapterService::new(DeviceKeyAdapterImpl))
        .await;
    let keys = container
        .get_type::<DeviceKeyAdapterService>()
        .await
        .expect("DeviceKeyAdapterService was just registered");

    container
        .set_type(FactoryProvisioningAdapterService::new(
            FactoryGatewayAdapterImpl,
        ))
        .await;
    let provisioning = container
        .get_type::<FactoryProvisioningAdapterService>()
        .await
        .expect("FactoryProvisioningAdapterService was just registered");

    container
        .set_type(CredentialWriterAdapterService::new(
            CredentialWriterAdapterImpl,
        ))
        .await;
    let credentials = container
        .get_type::<CredentialWriterAdapterService>()
        .await
        .expect("CredentialWriterAdapterService was just registered");

    container
        .set_type(FactoryService::new(FactoryControllerImpl::new(
            context.clone(),
            keys,
            provisioning,
            credentials,
        )))
        .await;
    let factory = container
        .get_type::<FactoryService>()
        .await
        .expect("FactoryService was just registered");

    // The provisioning station issues through the factory vertical, as the
    // context its `serve` selects; its journal, hooks and operator console
    // are its own.
    container
        .set_type(JournalAdapterService::new(JsonlJournalImpl::new()))
        .await;
    let journal = container
        .get_type::<JournalAdapterService>()
        .await
        .expect("JournalAdapterService was just registered");

    container
        .set_type(HookRunnerAdapterService::new(ProcessHookRunnerImpl))
        .await;
    let hooks = container
        .get_type::<HookRunnerAdapterService>()
        .await
        .expect("HookRunnerAdapterService was just registered");

    container
        .set_type(OperatorAdapterService::new(TuiOperatorImpl))
        .await;
    let operator = container
        .get_type::<OperatorAdapterService>()
        .await
        .expect("OperatorAdapterService was just registered");

    container
        .set_type(StationService::new(StationControllerImpl::new(
            context.clone(),
            factory.clone(),
            journal,
            hooks,
            operator,
        )))
        .await;
    let station = container
        .get_type::<StationService>()
        .await
        .expect("StationService was just registered");

    // A hub's side of the station, for `station simulate`: its key and
    // credential through the factory vertical, the station over HTTP.
    container
        .set_type(StationClientAdapterService::new(
            HttpStationClientImpl::new(),
        ))
        .await;
    let station_client = container
        .get_type::<StationClientAdapterService>()
        .await
        .expect("StationClientAdapterService was just registered");

    container
        .set_type(ClaimService::new(ClaimControllerImpl::new(
            station_client,
            factory.clone(),
        )))
        .await;
    let claim = container
        .get_type::<ClaimService>()
        .await
        .expect("ClaimService was just registered");

    // Pure orchestration over the other verticals' already-wired services:
    // no adapter of its own.
    container
        .set_type(BatchService::new(BatchControllerImpl::new(
            convert.clone(),
            identity.clone(),
            config.clone(),
            image.clone(),
            squashfs.clone(),
            factory.clone(),
        )))
        .await;
    let batch = container
        .get_type::<BatchService>()
        .await
        .expect("BatchService was just registered");

    // Same releases repo and installers as rmra's own `update`.
    container
        .set_type(ReleaseFeedAdapterService::new(GithubReleaseFeedImpl::new(
            RELEASES_REPO,
        )))
        .await;
    let release_feed = container
        .get_type::<ReleaseFeedAdapterService>()
        .await
        .expect("ReleaseFeedAdapterService was just registered");

    container
        .set_type(InstallerAdapterService::new(DistInstallerImpl::new(
            RELEASES_REPO,
        )))
        .await;
    let installer = container
        .get_type::<InstallerAdapterService>()
        .await
        .expect("InstallerAdapterService was just registered");

    container
        .set_type(UpdateService::new(UpdateControllerImpl::new(
            release_feed,
            installer,
        )))
        .await;
    let update = container
        .get_type::<UpdateService>()
        .await
        .expect("UpdateService was just registered");

    Services {
        context,
        ota,
        disk,
        flash,
        image,
        squashfs,
        identity,
        config,
        convert,
        batch,
        factory,
        station,
        claim,
        update,
    }
}
