use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    if env::var_os("CARGO_FEATURE_UELIB_BRIDGE").is_some() {
        build_uelib_bridge(&manifest_dir);
    }
}

const WITHOUT_BRIDGE: &str =
    "To build without subtitle matching, disable the bridge with `--no-default-features`.";

fn build_uelib_bridge(manifest_dir: &Path) {
    let target = env::var("TARGET").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    let (runtime, platform) = match target.as_str() {
        "x86_64-pc-windows-msvc" => ("win-x64", Platform::Windows),
        "aarch64-pc-windows-msvc" => ("win-arm64", Platform::Windows),
        "x86_64-unknown-linux-gnu" => ("linux-x64", Platform::Linux),
        "aarch64-unknown-linux-gnu" => ("linux-arm64", Platform::Linux),
        "aarch64-apple-darwin" => ("osx-arm64", Platform::MacOS),
        "x86_64-apple-darwin" => ("osx-x64", Platform::MacOS),
        _ => panic!(
            "The UELib Native AOT bridge does not support the {target} target. {WITHOUT_BRIDGE}"
        ),
    };
    // Native AOT can cross-compile between architectures, but not between
    // operating systems.
    if !platform.is_host(&host) {
        let name = platform.name();
        panic!(
            "Build the {name} UELib Native AOT bridge on {name}, not via cross-compilation from {host}. {WITHOUT_BRIDGE}"
        );
    }

    let project = manifest_dir.join("uelib-bridge/UELibBridge.csproj");
    println!("cargo:rerun-if-changed={}", project.display());
    for source in ["Exports.cs", "DirectPairs.cs"] {
        let source = manifest_dir.join("uelib-bridge").join(source);
        println!("cargo:rerun-if-changed={}", source.display());
    }

    let status = Command::new("dotnet")
        .arg("publish")
        .arg(&project)
        .arg("--configuration")
        .arg("Release")
        .arg("--runtime")
        .arg(runtime)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "Could not start dotnet to build the {runtime} UELib bridge: {error}. Install the .NET 10 SDK for this host and ensure `dotnet` is on PATH; in WSL, install the Linux SDK inside the distro. {WITHOUT_BRIDGE}"
            )
        });
    if !status.success() {
        panic!("Could not build the UELib Native AOT bridge. {WITHOUT_BRIDGE}");
    }

    let framework = "net10.0";
    let output_dir = manifest_dir
        .join("uelib-bridge/bin/Release")
        .join(framework)
        .join(runtime);
    let publish_dir = manifest_dir
        .join("uelib-bridge/bin/Release")
        .join(framework)
        .join(runtime)
        .join("publish");
    let (link_dir, library, runtime_library) = match platform {
        Platform::Windows => (
            output_dir.join("native"),
            output_dir.join("native/SCModUELibBridge.lib"),
            publish_dir.join("SCModUELibBridge.dll"),
        ),
        Platform::Linux => prepare_linker_library(&publish_dir, "so"),
        Platform::MacOS => prepare_linker_library(&publish_dir, "dylib"),
    };
    if !library.is_file() || !runtime_library.is_file() {
        panic!(
            "The UELib bridge build did not produce {} and {}.",
            library.display(),
            runtime_library.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", link_dir.display());
    println!("cargo:rustc-link-lib=dylib=SCModUELibBridge");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("Cargo OUT_DIR should be under target/<profile>/build");
    for destination in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
        fs::create_dir_all(&destination).unwrap_or_else(|error| {
            panic!(
                "Could not create UELib bridge runtime directory {}: {error}",
                destination.display()
            )
        });
        let destination = destination.join(
            runtime_library
                .file_name()
                .expect("bridge runtime library has a filename"),
        );
        fs::copy(&runtime_library, &destination).unwrap_or_else(|error| {
            panic!(
                "Could not copy the UELib bridge to {}: {error}",
                destination.display()
            )
        });
    }
}

#[derive(Clone, Copy)]
enum Platform {
    Windows,
    Linux,
    MacOS,
}

impl Platform {
    fn name(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::MacOS => "macOS",
        }
    }

    fn is_host(self, host: &str) -> bool {
        match self {
            Platform::Windows => host.contains("windows"),
            Platform::Linux => host.contains("linux"),
            Platform::MacOS => host.contains("apple"),
        }
    }
}

/// Copies the published library to a `lib`-prefixed name the linker can
/// find, returning the link directory, linker library, and runtime library.
fn prepare_linker_library(publish_dir: &Path, extension: &str) -> (PathBuf, PathBuf, PathBuf) {
    let published_library = publish_dir.join(format!("SCModUELibBridge.{extension}"));
    let linker_library = publish_dir.join(format!("libSCModUELibBridge.{extension}"));
    if !published_library.is_file() {
        panic!(
            "The UELib bridge build did not produce {}.",
            published_library.display()
        );
    }
    fs::copy(&published_library, &linker_library).unwrap_or_else(|error| {
        panic!(
            "Could not prepare the UELib bridge for linking at {}: {error}",
            linker_library.display()
        )
    });
    (
        publish_dir.to_path_buf(),
        linker_library.clone(),
        linker_library,
    )
}
