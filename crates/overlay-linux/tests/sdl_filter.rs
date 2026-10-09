#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::{path::Path, process::Command};

fn output(command: &mut Command) -> std::process::Output {
    command.output().unwrap()
}

#[test]
fn native_sdl_filter_preserves_replaced_and_removed_callback_state() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let source = fixtures.join("sdl_filter_driver.c");
    let directory = tempfile::tempdir().unwrap();
    let overlay = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("libvosulay_overlay.so");
    assert!(
        overlay.is_file(),
        "product overlay missing: {}",
        overlay.display()
    );
    let lookup = overlay.with_file_name("libvosulay_lookup.so");
    let endpoint = directory.path().join("unused-overlay-endpoint");
    let mut exercised = 0;

    for package in ["sdl2", "sdl3"] {
        let flags = output(Command::new("pkg-config").args(["--cflags", "--libs", package]));
        if !flags.status.success() {
            eprintln!("skipping {package}: native development package unavailable");
            continue;
        }
        let executable = directory.path().join(format!("sdl-filter-{package}"));
        let flags = String::from_utf8(flags.stdout).unwrap();
        let mut compile = Command::new("cc");
        compile
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .args(flags.split_whitespace())
            .arg("-o")
            .arg(&executable);
        if package == "sdl3" {
            compile.arg("-DVOSULAY_TEST_SDL3");
        }
        let result = output(&mut compile);
        assert!(
            result.status.success(),
            "{package} compile failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );

        let result = Command::new(&executable)
            .env(
                "LD_PRELOAD",
                format!("{}:{}", lookup.display(), overlay.display()),
            )
            .env("VOSULAY_ENDPOINT", &endpoint)
            .env("SDL_VIDEODRIVER", "dummy")
            .env("SDL_AUDIODRIVER", "dummy")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{package} native filter regression failed (exit {:?}):\n{}\n{}",
            result.status.code(),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        exercised += 1;
    }
    if exercised == 0 {
        eprintln!(
            "skipping native SDL filter regression: neither SDL2 nor SDL3 development package is available via pkg-config"
        );
    }
}
