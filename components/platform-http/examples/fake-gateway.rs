//! Serves the in-process fake platform gateway on its own, to try rmra's
//! commands by hand: prints how to point a context at it, then serves
//! until Ctrl-C. Any token logs in.
//!
//!   cargo run -p remora-platform-http --features test-gateway --example fake-gateway

use remora_platform_http::test_gateway::TestGateway;

#[tokio::main]
async fn main() {
    let (_gateway, context) = TestGateway::serve().await;
    println!(
        "rmra context create fake --address {} --plaintext --use \
         && echo t | rmra login --token-stdin",
        context.context.endpoint.address
    );
    tokio::signal::ctrl_c().await.unwrap();
}
