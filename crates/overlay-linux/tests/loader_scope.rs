#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::{path::Path, process::Command};

fn compile(source: &Path, output: &Path, shared: bool, dependency: Option<&Path>) {
    let mut compiler = Command::new("cc");
    compiler.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
    if shared {
        compiler.args(["-fPIC", "-shared"]);
    }
    if let Some(dependency) = dependency {
        // RTLD_NEXT searches the caller's loader group, including dependencies.
        compiler.arg("-Wl,--no-as-needed").arg(dependency);
    }
    let result = compiler
        .arg(source)
        .arg("-ldl")
        .arg("-o")
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn preload_preserves_native_caller_scope_and_explicit_library_identity() {
    let directory = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let caller = directory.path().join("libcaller.so");
    let provider = directory.path().join("libprovider.so");
    let driver = directory.path().join("loader-driver");
    compile(&fixtures.join("loader_provider.c"), &provider, true, None);
    compile(
        &fixtures.join("loader_caller.c"),
        &caller,
        true,
        Some(&provider),
    );
    compile(&fixtures.join("loader_driver.c"), &driver, false, None);
    let executable = std::env::current_exe().unwrap();
    let overlay = executable.parent().unwrap().join("libvosulay_overlay.so");
    for scope in ["next", "local", "explicit"] {
        let result = Command::new(&driver)
            .args([scope, caller.to_str().unwrap(), provider.to_str().unwrap()])
            .arg(&overlay)
            .env("LD_PRELOAD", &overlay)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{scope}: exit {:?}\n{}\n{}",
            result.status.code(),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
