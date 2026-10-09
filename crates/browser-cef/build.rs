use std::{env, path::PathBuf};
use vosulay_cef_tools::{PINS_PATH, acquire_verified, default_cache, verify_runtime};

fn main() {
    for name in ["CEF_PATH", "XDG_CACHE_HOME", "HOME", "USERPROFILE"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed={PINS_PATH}");
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
        "x86_64",
        "pinned CEF distributions support x86_64 targets"
    );
    let windows = match env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "windows" => true,
        "linux" => false,
        other => panic!("no pinned CEF distribution for target OS: {other}"),
    };
    let platform = if windows {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    };
    let result = if let Some(path) = env::var_os("CEF_PATH") {
        let root = PathBuf::from(path);
        verify_runtime(&root, platform).map(|receipt| (root, receipt))
    } else {
        default_cache("cef").and_then(|cache| acquire_verified(platform, &cache))
    };
    let (runtime, receipt) = result.unwrap_or_else(|error| {
        panic!(
            "Pinned CEF setup failed: {error:#}. \
             Set CEF_PATH to a verified pinned distribution to override acquisition."
        )
    });
    let runtime = runtime
        .canonicalize()
        .expect("canonical verified CEF directory");
    let installed_runtime = if windows {
        runtime.clone()
    } else {
        vosulay_cef_tools::default_cache("runtime")
            .and_then(|cache| vosulay_cef_tools::cache_runtime(&runtime, &receipt, &cache))
            .expect("retain verified executable-adjacent CEF runtime")
    };
    println!("cargo::metadata=runtime={}", installed_runtime.display());
    println!(
        "cargo:rerun-if-changed={}",
        runtime.join(".vosulay-cef-verified.json").display()
    );
    for directory in ["Release", "Resources"] {
        println!(
            "cargo:rerun-if-changed={}",
            runtime.join(directory).display()
        );
    }
    for relative in receipt.runtime_sha256.keys() {
        println!(
            "cargo:rerun-if-changed={}",
            runtime.join(relative).display()
        );
        if !windows {
            let name = relative
                .strip_prefix("Release/")
                .or_else(|| relative.strip_prefix("Resources/"))
                .expect("verified runtime file directory");
            println!(
                "cargo:rerun-if-changed={}",
                installed_runtime.join(name).display()
            );
        }
    }
    println!(
        "cargo:rustc-link-search=native={}",
        runtime.join("Release").display()
    );
    println!(
        "cargo:rustc-link-lib=dylib={}",
        if windows { "libcef" } else { "cef" }
    );
    if !windows {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
