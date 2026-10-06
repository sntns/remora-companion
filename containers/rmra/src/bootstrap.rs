use remora_channel::{
    adapter::{gateway::ChannelGatewayAdapterService, ssh::SshClientAdapterService},
    application::ChannelService,
};
use remora_channel_adapter_grpc::GatewayAdapterImpl;
use remora_channel_adapter_openssh::OpenSshClientImpl;
use remora_channel_application::ChannelControllerImpl;
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
use remora_device::{adapter::gateway::DeviceGatewayAdapterService, application::DeviceService};
use remora_device_adapter_grpc::DeviceGatewayAdapterImpl;
use remora_device_application::DeviceControllerImpl;
use remora_ota::{
    adapter::{gateway::OtaGatewayAdapterService, source::ArtifactSourceAdapterService},
    application::OtaService,
};
use remora_ota_adapter_file::FileArtifactSourceImpl;
use remora_ota_adapter_grpc::OtaGatewayAdapterImpl;
use remora_ota_application::OtaControllerImpl;
use remora_update::{
    adapter::{feed::ReleaseFeedAdapterService, installer::InstallerAdapterService},
    application::UpdateService,
};
use remora_update_adapter_github::{DistInstallerImpl, GithubReleaseFeedImpl, RELEASES_REPO};
use remora_update_application::UpdateControllerImpl;

pub struct Services {
    pub context: ContextService,
    pub channel: ChannelService,
    pub ota: OtaService,
    pub device: DeviceService,
    pub update: UpdateService,
}

/// The composition root, same recipe as remora-etcher's: construct each
/// adapter, register it, pull it back out for the use case that needs it.
pub async fn wire() -> Services {
    let container = busybody::ServiceContainerBuilder::new().build();
    let root = default_root();

    container
        .set_type(ContextStoreAdapterService::new(FileContextStoreImpl::new(
            &root,
        )))
        .await;
    let store = container
        .get_type::<ContextStoreAdapterService>()
        .await
        .expect("ContextStoreAdapterService was just registered");

    container
        .set_type(CredentialStoreAdapterService::new(
            FileCredentialStoreImpl::new(&root),
        ))
        .await;
    let credentials = container
        .get_type::<CredentialStoreAdapterService>()
        .await
        .expect("CredentialStoreAdapterService was just registered");

    container
        .set_type(PlatformSessionAdapterService::new(
            PlatformSessionAdapterImpl,
        ))
        .await;
    let platform = container
        .get_type::<PlatformSessionAdapterService>()
        .await
        .expect("PlatformSessionAdapterService was just registered");

    container
        .set_type(ContextService::new(ContextControllerImpl::new(
            store,
            credentials,
            platform,
        )))
        .await;
    let context = container
        .get_type::<ContextService>()
        .await
        .expect("ContextService was just registered");

    container
        .set_type(ChannelGatewayAdapterService::new(GatewayAdapterImpl))
        .await;
    let gateway = container
        .get_type::<ChannelGatewayAdapterService>()
        .await
        .expect("ChannelGatewayAdapterService was just registered");

    container
        .set_type(SshClientAdapterService::new(OpenSshClientImpl))
        .await;
    let ssh = container
        .get_type::<SshClientAdapterService>()
        .await
        .expect("SshClientAdapterService was just registered");

    container
        .set_type(ChannelService::new(ChannelControllerImpl::new(
            context.clone(),
            gateway,
            ssh,
        )))
        .await;
    let channel = container
        .get_type::<ChannelService>()
        .await
        .expect("ChannelService was just registered");

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

    // OTA after device: a selector picks its devices through DeviceService.
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
        .set_type(OtaService::new(OtaControllerImpl::new(
            context.clone(),
            device.clone(),
            ota_gateway,
            artifacts,
        )))
        .await;
    let ota = container
        .get_type::<OtaService>()
        .await
        .expect("OtaService was just registered");

    container
        .set_type(ReleaseFeedAdapterService::new(GithubReleaseFeedImpl::new(
            RELEASES_REPO,
        )))
        .await;
    let feed = container
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
            feed, installer,
        )))
        .await;
    let update = container
        .get_type::<UpdateService>()
        .await
        .expect("UpdateService was just registered");

    Services {
        context,
        channel,
        ota,
        device,
        update,
    }
}
