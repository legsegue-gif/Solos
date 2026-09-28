//! Carries the guest CLI (crates/guest) into the core. `ios/build-core.sh`
//! cross-compiles it first; a build script does not run cargo itself. When
//! it has not been built (a desktop test run), an empty payload is carried
//! and nothing is installed in the guest.

use std::path::PathBuf;

const TARGET: &str = "aarch64-unknown-linux-musl";

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("solos-guest");
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../target"));
    let built = target_dir.join(TARGET).join("release").join("solos");
    println!("cargo:rerun-if-changed={}", built.display());
    let bytes = std::fs::read(&built).unwrap_or_default();
    std::fs::write(&out, &bytes).unwrap();
}
