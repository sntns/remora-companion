use std::sync::Arc;

use remora_etcher_batch::application::BatchService;
use remora_etcher_batch_application::BatchControllerImpl;
use remora_etcher_config::application::ConfigService;
use remora_etcher_config_application::ConfigControllerImpl;
use remora_etcher_convert::application::ConvertService;
use remora_etcher_convert_adapter_gzip::GzipAdapterImpl;
use remora_etcher_convert_adapter_qcow2::Qcow2AdapterImpl;
use remora_etcher_convert_application::ConvertControllerImpl;
use remora_etcher_disk::{adapter::DiskAdapterService, application::DiskService};
use remora_etcher_disk_adapter_native::DiskAdapterImpl;
use remora_etcher_disk_application::DiskControllerImpl;
use remora_etcher_factory::{
    adapter::FactoryProvisioningAdapterService, application::FactoryService,
};
use remora_etcher_factory_adapter_gateway::GatewayAdapterImpl;
use remora_etcher_factory_application::FactoryControllerImpl;
use remora_etcher_flash::{adapter::BmapAdapterService, application::FlashService};
use remora_etcher_flash_adapter_bmap::BmapAdapterImpl;
use remora_etcher_flash_application::FlashControllerImpl;
use remora_etcher_identity::{adapter::KeygenAdapterService, application::IdentityService};
use remora_etcher_identity_adapter_keygen::KeygenAdapterImpl;
use remora_etcher_identity_application::IdentityControllerImpl;
use remora_etcher_image::adapter::ext4::Ext4AdapterService;
use remora_etcher_image::{
    adapter::partition_table::PartitionTableAdapterService, application::ImageService,
};
use remora_etcher_image_adapter_ext4::Ext4AdapterImpl;
use remora_etcher_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_etcher_image_adapter_vfat::VfatAdapterImpl;
use remora_etcher_image_application::ImageControllerImpl;
use remora_etcher_squashfs::{adapter::SquashfsAdapterService, application::SquashfsService};
use remora_etcher_squashfs_adapter_backhand::SquashfsAdapterImpl;
use remora_etcher_squashfs_application::SquashfsControllerImpl;

/// Every service the CLI can dispatch into, wired once at startup. All
/// fields deref all the way down to `dyn ...ServiceInterface` (`DiskService`
/// -> `Arc<Box<dyn DiskServiceInterface>>` -> `Box<dyn DiskServiceInterface>`
/// -> `dyn DiskServiceInterface`), so call sites just do `services.disk.list()`.
pub struct Services {
    pub disk: DiskService,
    pub flash: FlashService,
    pub image: ImageService,
    pub squashfs: SquashfsService,
    pub identity: IdentityService,
    pub config: ConfigService,
    pub convert: ConvertService,
    pub batch: BatchService,
    pub factory: FactoryService,
}

/// The composition root: construct each adapter, register it, pull it back
/// out to hand to the use case that depends on it, register that use case in
/// turn. Same recipe as remora-edge's `containers/remora-*d/src/daemon.rs`.
///
/// Wiring itself needs an async context purely because `busybody`'s
/// container API is async (so it can, elsewhere, resolve dependencies that
/// really do need to await something); nothing past this function is async —
/// the CLI dispatch stays plain synchronous Rust.
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
        .set_type(BmapAdapterService::new(BmapAdapterImpl))
        .await;
    let bmap_adapter = container
        .get_type::<BmapAdapterService>()
        .await
        .expect("BmapAdapterService was just registered");

    container
        .set_type(FlashService::new(FlashControllerImpl::new(bmap_adapter)))
        .await;
    let flash = container
        .get_type::<FlashService>()
        .await
        .expect("FlashService was just registered");

    container
        .set_type(PartitionTableAdapterService::new(PartitionTableAdapterImpl))
        .await;
    let partition_table = container
        .get_type::<PartitionTableAdapterService>()
        .await
        .expect("PartitionTableAdapterService was just registered");

    container
        .set_type(remora_etcher_fs_walk::FsWalkAdapterService::new(
            remora_etcher_fs_walk::FsWalkAdapterImpl,
        ))
        .await;
    let fs_walk = container
        .get_type::<remora_etcher_fs_walk::FsWalkAdapterService>()
        .await
        .expect("FsWalkAdapterService was just registered");

    // ext4_fs/vfat_fs are consumed by exactly one constructor right below,
    // so there's no ambiguity to disambiguate via the container — skip the
    // set_type/get_type round-trip for these two (busybody keys purely on
    // TypeId, and `Arc<dyn PartitionFilesystem>` would collide between them
    // if both went through it under the same type).
    let ext4_fs: Arc<dyn remora_etcher_image::adapter::partition_fs::PartitionFilesystem> =
        Arc::new(Ext4AdapterImpl);
    let vfat_fs: Arc<dyn remora_etcher_image::adapter::partition_fs::PartitionFilesystem> =
        Arc::new(VfatAdapterImpl);

    container
        .set_type(ImageService::new(ImageControllerImpl::new(
            partition_table,
            ext4_fs,
            vfat_fs,
            fs_walk.clone(),
        )))
        .await;
    let image = container
        .get_type::<ImageService>()
        .await
        .expect("ImageService was just registered");

    // The config vertical injects Ext4Adapter directly too (not through
    // PartitionFilesystem) to manipulate a standalone config.ext4 file —
    // see remora-etcher-image's Ext4Adapter doc comment.
    container
        .set_type(Ext4AdapterService::new(Ext4AdapterImpl))
        .await;
    let ext4_adapter = container
        .get_type::<Ext4AdapterService>()
        .await
        .expect("Ext4AdapterService was just registered");

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
        .get_type::<remora_etcher_fs_walk::FsWalkAdapterService>()
        .await
        .expect("FsWalkAdapterService was registered earlier");

    container
        .set_type(ConfigService::new(ConfigControllerImpl::new(
            ext4_adapter,
            fs_walk_for_config,
            image.clone(),
        )))
        .await;
    let config = container
        .get_type::<ConfigService>()
        .await
        .expect("ConfigService was just registered");

    // The convert vertical injects both format adapters directly (like the
    // image vertical's ext4_fs/vfat_fs above), not through the busybody
    // container: ContainerFormatAdapter is implemented by two distinct
    // concrete types at once, which would collide under the same wrapper
    // TypeId if registered there.
    let qcow2_adapter: Arc<dyn remora_etcher_convert::adapter::ContainerFormatAdapter> =
        Arc::new(Qcow2AdapterImpl);
    let gzip_adapter: Arc<dyn remora_etcher_convert::adapter::ContainerFormatAdapter> =
        Arc::new(GzipAdapterImpl);

    container
        .set_type(ConvertService::new(ConvertControllerImpl::new(
            qcow2_adapter,
            gzip_adapter,
        )))
        .await;
    let convert = container
        .get_type::<ConvertService>()
        .await
        .expect("ConvertService was just registered");

    // Pure orchestration over the other verticals' already-wired services —
    // no adapter of its own, so no set_type/get_type round-trip needed;
    // just construct it directly like the ext4_fs/vfat_fs handles above.
    let batch = BatchService::new(BatchControllerImpl::new(
        convert.clone(),
        identity.clone(),
        config.clone(),
        image.clone(),
        squashfs.clone(),
    ));

    // GatewayAdapterImpl is stateless (gateway URL/API key are CLI-level
    // config, passed per call -- see FactoryProvisioningAdapter's doc
    // comment), so it needs no set_type/get_type round-trip either.
    let factory = FactoryService::new(FactoryControllerImpl::new(
        FactoryProvisioningAdapterService::new(GatewayAdapterImpl::default()),
        image.clone(),
    ));

    Services {
        disk,
        flash,
        image,
        squashfs,
        identity,
        config,
        convert,
        batch,
        factory,
    }
}
