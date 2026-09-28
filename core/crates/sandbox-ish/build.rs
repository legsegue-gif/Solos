//! Compiles the C shim against the iSH source tree and links the kernel
//! libraries built by `deps/build_ish.sh`.
//!
//! On anything but iOS the crate still compiles, with every entry point
//! reporting that the sandbox is unavailable, so the workspace builds and
//! tests everywhere.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/c/solos_ish.c");
    println!("cargo:rerun-if-changed=src/c/solos_ish.h");
    println!("cargo:rustc-check-cfg=cfg(ish_stub)");

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("apple-ios") {
        println!("cargo:rustc-cfg=ish_stub");
        return;
    }
    let sdk = if target.ends_with("-sim") { "iphonesimulator" } else { "iphoneos" };
    let deps = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../deps");
    let ish = deps.join("ish");
    let build = ish.join(format!("build-{sdk}"));
    let lib = deps.join(format!("out/ios-{sdk}/lib"));
    if !lib.join("libish.a").exists() {
        panic!("iSH is not built for {sdk}: run deps/build_ish.sh --sdk {sdk}");
    }

    cc::Build::new()
        .file("src/c/solos_ish.c")
        .include("src/c")
        .include(&ish)
        .include(&build)
        .define("ISH_INTERNAL", None)
        // Darwin gates the ucontext routines the crash handler needs.
        .define("_XOPEN_SOURCE", "700")
        .define("_DARWIN_C_SOURCE", None)
        .define("GUEST_ARM64", "1")
        .define("ENGINE_ASBESTOS", "1")
        .define("KERNEL_ISH", "1")
        .warnings(false)
        .compile("solos_ish_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    for l in ["ish", "ish_emu", "fakefs"] {
        println!("cargo:rustc-link-lib=static={l}");
    }
    println!("cargo:rustc-link-lib=dylib=sqlite3");
    println!("cargo:rustc-link-lib=dylib=resolv");
    println!("cargo:rustc-link-lib=framework=Network");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
