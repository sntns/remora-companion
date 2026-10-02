/// Compiles the vendored protos (see proto/README.md) with protox, a pure-Rust
/// protobuf compiler, so building needs no protoc. Servers are generated too:
/// adapter crates' tests stand up an in-process fake gateway with them.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files = [
        "sntns/service/remorachannel/v1/channel_service.proto",
        "sntns/service/iam/v1/user_service.proto",
        "sntns/service/iam/v1/account_service.proto",
    ];
    let descriptors = protox::compile(files, ["proto"])?;
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
