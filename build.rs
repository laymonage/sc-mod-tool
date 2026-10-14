use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The UELib bridge library is shipped beside the executable. Windows and
    // macOS find it there already, but Linux only looks there with an rpath.
    // This has to be set here, because link arguments from core's build script
    // don't apply to this crate's binary.
    if env::var_os("CARGO_FEATURE_UELIB_BRIDGE").is_some()
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
    {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
