use sha2::{Digest, Sha256};
use std::{env, fs, io::Read, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    let runtime = PathBuf::from(env::var_os("CEF_PATH").expect(
        "Run tools/cef/acquire.py and set CEF_PATH to its verified distribution; CEF is never downloaded by Cargo",
    ));
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let platform = if windows {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    };
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("../../tools/cef/pins.json")).unwrap();
    let receipt_path = runtime.join(".vosulay-cef-verified.json");
    println!("cargo:rerun-if-changed={}", receipt_path.display());
    let receipt: serde_json::Value = serde_json::from_slice(
        &fs::read(&receipt_path)
            .expect("CEF acquisition receipt missing; run tools/cef/acquire.py"),
    )
    .unwrap();
    assert_eq!(
        receipt["platform"], platform,
        "CEF target platform mismatch"
    );
    assert_eq!(
        receipt["cef_version"], pins["cef_version"],
        "CEF version mismatch"
    );
    assert_eq!(
        receipt["archive_sha256"], pins["platforms"][platform]["sha256"],
        "CEF archive pin mismatch"
    );
    for (relative, expected) in receipt["runtime_sha256"]
        .as_object()
        .expect("CEF runtime manifest missing")
    {
        let path = runtime.join(relative);
        assert!(
            !path.is_symlink()
                && path
                    .canonicalize()
                    .unwrap()
                    .starts_with(runtime.canonicalize().unwrap()),
            "unsafe CEF manifest path"
        );
        println!("cargo:rerun-if-changed={}", path.display());
        let mut file = fs::File::open(&path).unwrap();
        let mut buffer = [0_u8; 65536];
        let mut hash = Sha256::new();
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        assert_eq!(
            format!("{:x}", hash.finalize()),
            expected.as_str().unwrap(),
            "CEF runtime digest mismatch: {relative}"
        );
    }
    let release = if runtime.join("Release").is_dir() {
        runtime.join("Release")
    } else {
        runtime.clone()
    };
    let library = release.join(if windows { "libcef.lib" } else { "libcef.so" });
    assert!(
        library.is_file(),
        "CEF library missing: {}",
        library.display()
    );
    println!("cargo:rustc-link-search=native={}", release.display());
    println!(
        "cargo:rustc-link-lib=dylib={}",
        if windows { "libcef" } else { "cef" }
    );
    if !windows {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
