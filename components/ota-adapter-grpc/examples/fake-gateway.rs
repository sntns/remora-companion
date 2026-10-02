//! Serves the in-process fake remora gateway on its own, to try rmra's ota
//! commands by hand: prints a context to create, then serves until Ctrl-C.
//!
//!   cargo run -p remora-ota-adapter-grpc --features test-gateway --example fake-gateway

use remora_ota_adapter_grpc::test_gateway::TestGateway;

#[tokio::main]
async fn main() {
    let (_gateway, context) = TestGateway::serve().await;
    println!(
        "rmra context create fake --address {} --plaintext --use \
         && echo t | rmra login --token-stdin   # login needs IAM: write credentials.json instead",
        context.context.endpoint.address
    );
    println!("{}", context.context.endpoint.address);
    tokio::signal::ctrl_c().await.unwrap();
}
