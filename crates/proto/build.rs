use std::error::Error;
use std::path::Path;

const FILES: [&str; 1] = ["bosun/agent/v1/agent.proto"];

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    println!("cargo:rerun-if-changed={}", root.display());
    let descriptors = protox::compile(FILES, [&root])?;
    tonic_prost_build::configure()
        .build_transport(false)
        .compile_fds(descriptors)?;
    Ok(())
}
