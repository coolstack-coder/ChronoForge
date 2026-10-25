// protox compiles the .proto in pure Rust, so nobody needs protoc installed.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../proto/chronoforge.proto";
    println!("cargo:rerun-if-changed={proto}");
    let fds = protox::compile([proto], ["../../proto"])?;
    tonic_build::configure().compile_fds(fds)?;
    Ok(())
}
