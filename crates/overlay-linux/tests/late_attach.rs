#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::{path::Path, process::Command};

fn compile(arguments: &[&str], output: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let result = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(&fixtures)
        .args(arguments.iter().map(|argument| {
            if argument.ends_with(".c") {
                fixtures.join(argument).into_os_string()
            } else {
                argument.into()
            }
        }))
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

/// Late attach must walk every loaded module. Ordinary libraries with
/// over-long imported names (Mesa's libLLVM) or more than 65536 relocations
/// (NVIDIA's EGL core) used to abort and roll back the whole attach.
#[test]
fn late_attach_tolerates_long_names_and_large_relocation_tables() {
    let directory = tempfile::tempdir().unwrap();
    let provider = directory.path().join("libvosulay-late-provider.so");
    let driver = directory.path().join("late-attach-driver");
    compile(&["-fPIC", "-shared", "late_attach_provider.c"], &provider);
    compile(&["late_attach_driver.c", "-ldl"], &driver);
    let overlay = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("libvosulay_overlay.so");
    for variant in ["VOSULAY_LONG_IMPORT", "VOSULAY_MANY_RELOCATIONS"] {
        let importer = directory.path().join(format!("lib{variant}.so"));
        compile(
            &[
                "-fPIC",
                "-shared",
                &format!("-D{variant}"),
                "late_attach_importer.c",
                "-Wl,--no-as-needed",
                provider.to_str().unwrap(),
            ],
            &importer,
        );
        let endpoint = directory.path().join("overlay.sock");
        let result = Command::new(&driver)
            .arg(&importer)
            .arg(&overlay)
            .arg(&endpoint)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{variant}: exit {:?}\n{}\n{}",
            result.status.code(),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
