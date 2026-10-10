use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for name in [
        "XDG_CACHE_HOME",
        "HOME",
        "USERPROFILE",
        "VOSULAY_PACKAGE_BUILD",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Packaging and code signing need the DLL's own version metadata;
        // it must be present before signing, so it is linked in here.
        let resource = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"))
            .join("vosulay-version.res");
        std::fs::write(
            &resource,
            vosulay_cef_tools::package::windows_dll_version_resource("vosulay.dll")
                .expect("vosulay.dll version resource"),
        )
        .expect("write vosulay.dll version resource");
        println!("cargo:rustc-link-arg-cdylib={}", resource.display());
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        if env::var_os("CARGO_FEATURE_CEF_HOST").is_some() {
            let runtime = PathBuf::from(
                env::var_os("DEP_VOSULAY_CEF_RUNTIME").expect("verified CEF dependency metadata"),
            );
            println!("cargo:rustc-env=VOSULAY_CEF_RUNTIME={}", runtime.display());
            // Cargo only installs the executable. Portable packages still take
            // precedence; local builds retain the verified runtime search path.
            // Distribution packages always ship libcef beside the executable and
            // must not search the build machine's cache.
            if env::var_os("VOSULAY_PACKAGE_BUILD").is_none() {
                println!("cargo:rustc-link-arg=-Xlinker");
                println!("cargo:rustc-link-arg=--rpath={}", runtime.display());
            }
            #[cfg(all(target_os = "linux", feature = "cef-host"))]
            for (variable, output, library) in [
                (
                    "VOSULAY_OVERLAY_LIBRARY",
                    vosulay_overlay::BUILD_OUTPUT_DIRECTORY,
                    "libvosulay_overlay.so",
                ),
                (
                    "VOSULAY_LOOKUP_LIBRARY",
                    vosulay_lookup::BUILD_OUTPUT_DIRECTORY,
                    "libvosulay_lookup.so",
                ),
            ] {
                println!(
                    "cargo:rustc-env={variable}={}",
                    retain_overlay(output, library)
                        .expect("retain built Linux overlay library")
                        .display()
                );
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "cef-host"))]
fn retain_overlay(output: &str, library: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::{fs, io::ErrorKind, path::Path};
    use vosulay_cef_tools::{default_cache, sha256};

    let profile = Path::new(output)
        .ancestors()
        .nth(3)
        .expect("Cargo build output directory");
    let source = profile.join("deps").join(library);
    println!("cargo:rerun-if-changed={}", source.display());
    let digest = sha256(&source)?;
    let cache = default_cache("overlay")?;
    fs::create_dir_all(&cache)?;
    let destination = cache.join(format!("{digest}.so"));
    if !destination.exists() {
        let temporary = tempfile::NamedTempFile::new_in(&cache)?;
        fs::copy(&source, temporary.path())?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
    }
    if sha256(&destination)? != digest {
        return Err(format!(
            "cached Linux compositor digest mismatch: {}",
            destination.display()
        )
        .into());
    }
    println!("cargo:rerun-if-changed={}", destination.display());
    Ok(destination.canonicalize()?)
}
