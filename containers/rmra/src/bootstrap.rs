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

pub struct Services {
    pub context: ContextService,
    pub channel: ChannelService,
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

    Services { context, channel }
}
