use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let vgmstream_root = manifest_dir.join("../vendor/vgmstream");
    let decoder = vgmstream_root.join("src/coding/ubi_adpcm_decoder.c");
    let adapter = manifest_dir.join("src/decoder/ubi_adpcm_native.c");
    if !decoder.is_file() {
        panic!(
            "vgmstream source not found at {}. Initialize the submodule with `git submodule update --init --recursive`.",
            vgmstream_root.display()
        );
    }

    println!("cargo:rerun-if-changed={}", decoder.display());
    println!("cargo:rerun-if-changed={}", adapter.display());

    let mut build = cc::Build::new();
    build
        .file(&adapter)
        .file(&decoder)
        .include(vgmstream_root.join("src"))
        .include(vgmstream_root.join("src/coding"))
        .include(vgmstream_root.join("ext_includes"));
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/std:c11");
    } else {
        build.flag("-std=c11");
    }
    build.compile("sc1_vgmstream_decoder");
}
