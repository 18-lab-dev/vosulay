use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{PRODUCT_VERSION, files, pins, verify_runtime};

#[path = "zip.rs"]
mod zip;

#[derive(Serialize)]
struct Manifest<'a> {
    product_version: &'a str,
    cef_version: &'a str,
    platform: &'a str,
    archive: &'a str,
    archive_sha256: &'a str,
    runtime_sha256: BTreeMap<String, String>,
}

const WINDOWS_ICON: &[u8] = include_bytes!("../../../assets/branding/vosulay.ico");

/// CEF's prebuilt sandbox bootstrap becomes the app executable, and Windows
/// names a process after its version resource, so Task Manager listed vosulay
/// as "CEF Bootstrap Application". CEF documents editing the bootstrap's icon
/// and file information. Its copyright line is kept: the code is still CEF's.
pub(crate) fn brand_windows_bootstrap(path: &Path) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("bootstrap path has no UTF-8 basename")?;
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut image =
        editpe::Image::parse(bytes.as_slice()).context("parsing bootstrap executable")?;
    let mut resources = image
        .resource_directory()
        .cloned()
        .context("bootstrap has no resources")?;
    let mut version = resources
        .get_version_info()?
        .context("bootstrap has no version resource")?;
    apply_version_metadata(&mut version, file_name)?;
    resources.set_version_info(&version)?;
    resources.set_main_icon(WINDOWS_ICON)?;
    image.set_resource_directory(resources)?;
    let mut branded = Vec::new();
    image.write_writer(&mut branded)?;
    fs::write(path, branded).with_context(|| format!("writing {}", path.display()))
}

/// Version fields Windows shows for every binary vosulay builds or brands.
/// SignPath requires the product name and version on each signed file.
fn apply_version_metadata(version: &mut editpe::VersionInfo, file_name: &str) -> Result<()> {
    let numeric = |index: usize| -> Result<u32> {
        let part = PRODUCT_VERSION
            .split(['.', '-', '+'])
            .nth(index)
            .unwrap_or("0");
        let value: u32 = part
            .parse()
            .with_context(|| format!("version part {part:?}"))?;
        ensure!(value <= 0xffff, "version part {value} exceeds 65535");
        Ok(value)
    };
    let numbers = editpe::types::VersionU32 {
        major: (numeric(0)? << 16) | numeric(1)?,
        minor: numeric(2)? << 16,
    };
    version.info.file_version = numbers;
    version.info.product_version = numbers;
    for table in &mut version.strings {
        for (key, value) in [
            ("CompanyName", "sxlvia"),
            ("FileDescription", "Vosulay"),
            ("ProductName", "Vosulay"),
            ("InternalName", "vosulay"),
            ("OriginalFilename", file_name),
            ("FileVersion", PRODUCT_VERSION),
            ("ProductVersion", PRODUCT_VERSION),
        ] {
            table.strings.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(())
}

/// A compiled resource file (`.res`) with the version block of a DLL that
/// vosulay builds; `link.exe` and `lld-link` embed `.res` inputs directly.
/// Writing it here rather than through a `.rc` script avoids requiring a
/// resource compiler (`rc.exe` or `llvm-rc`) on every build machine.
pub fn windows_dll_version_resource(file_name: &str) -> Result<Vec<u8>> {
    const VFT_DLL: u32 = 2;
    const RT_VERSION: u16 = 16;
    const LANG_EN_US: u16 = 0x0409;
    const UTF16: u16 = 1200;
    const MOVEABLE_PURE: u16 = 0x0030;

    let mut version = editpe::VersionInfo {
        info: editpe::types::FixedFileInfo {
            file_type: VFT_DLL,
            ..Default::default()
        },
        strings: vec![editpe::VersionStringTable {
            key: format!("{LANG_EN_US:04X}{UTF16:04X}"),
            strings: Default::default(),
        }],
        vars: vec![editpe::types::VersionU16 {
            major: LANG_EN_US,
            minor: UTF16,
        }],
    };
    apply_version_metadata(&mut version, file_name)?;
    let data = version.try_build()?;

    let mut resource = Vec::with_capacity(64 + data.len());
    // Every .res file opens with an empty entry that identifies the format.
    push_resource_entry(&mut resource, 0, 0, 0, 0, &[])?;
    push_resource_entry(
        &mut resource,
        RT_VERSION,
        1,
        LANG_EN_US,
        MOVEABLE_PURE,
        &data,
    )?;
    Ok(resource)
}

fn push_resource_entry(
    output: &mut Vec<u8>,
    kind: u16,
    name: u16,
    language: u16,
    flags: u16,
    data: &[u8],
) -> Result<()> {
    const HEADER_SIZE: u32 = 32;
    output.extend(u32::try_from(data.len())?.to_le_bytes());
    output.extend(HEADER_SIZE.to_le_bytes());
    for ordinal in [kind, name] {
        output.extend(0xffff_u16.to_le_bytes());
        output.extend(ordinal.to_le_bytes());
    }
    output.extend(0_u32.to_le_bytes()); // DataVersion
    output.extend(flags.to_le_bytes());
    output.extend(language.to_le_bytes());
    output.extend(0_u32.to_le_bytes()); // Version
    output.extend(0_u32.to_le_bytes()); // Characteristics
    output.extend(data);
    output.resize(output.len().next_multiple_of(4), 0);
    Ok(())
}

pub fn package_runtime(
    platform: &str,
    cef: &Path,
    output: &Path,
    executables: &[PathBuf],
    overlays: &[PathBuf],
    client: Option<&Path>,
) -> Result<()> {
    let receipt = verify_runtime(cef, platform)?;
    let pins = pins()?;
    let pin = pins
        .platforms
        .get(platform)
        .with_context(|| format!("unsupported CEF platform: {platform}"))?;

    if output.exists() {
        bail!("output already exists: {}", output.display());
    }
    if let Some(client) = client
        && (platform != "windows-x86_64"
            || !client
                .extension()
                .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("dll"))
            || !client.is_file()
            || client.symlink_metadata()?.file_type().is_symlink())
    {
        bail!(
            "--client requires a regular Windows client DLL: {}",
            client.display()
        );
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating package parent {}", parent.display()))?;
    }
    fs::create_dir(output)
        .with_context(|| format!("creating package directory {}", output.display()))?;

    let result = (|| -> Result<()> {
        for name in &pin.package_release_files {
            copy_file(&cef.join("Release").join(name), &output.join(name))?;
        }
        for name in &pin.package_resources {
            copy_tree(&cef.join("Resources").join(name), &output.join(name))?;
        }
        for name in ["LICENSE.txt", "CREDITS.html"] {
            copy_file(&cef.join(name), &output.join(name))?;
        }
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (source, name) in [
            ("LICENSE", "LICENSE-vosulay.txt"),
            ("assets/branding/LICENSE.md", "LICENSE-branding.txt"),
            ("THIRD_PARTY_NOTICES.md", "THIRD_PARTY_NOTICES.md"),
        ] {
            copy_file(&repository.join(source), &output.join(name))?;
        }

        for executable in executables {
            check_input(executable)?;
            if !executable.is_file() {
                bail!(
                    "executable input must be a regular file: {}",
                    executable.display()
                );
            }
            let basename = executable
                .file_name()
                .context("executable has no basename")?;
            copy_file(executable, &output.join(basename))?;
        }
        if let Some(client) = client {
            let client_name = client.file_name().context("client has no basename")?;
            let bootstrap_name = client
                .with_extension("exe")
                .file_name()
                .context("client has no basename")?
                .to_os_string();
            for (source, basename) in [
                (client.to_path_buf(), client_name.to_os_string()),
                (cef.join("Release/bootstrap.exe"), bootstrap_name.clone()),
            ] {
                copy_file(&source, &output.join(basename))?;
            }
            brand_windows_bootstrap(&output.join(&bootstrap_name))?;
        }
        for overlay in overlays {
            check_input(overlay)?;
            let basename = overlay.file_name().context("overlay has no basename")?;
            copy_tree(overlay, &output.join(basename))?;
        }
        // The pinned Linux libcef.so carries over 1 GB of debug and static
        // symbols; shipping them would make every package several times larger.
        if platform.starts_with("linux") {
            strip_symbols(output)?;
        }

        let manifest_path = output.join("vosulay-cef.json");
        if manifest_path.exists() {
            bail!("duplicate reserved package basename: vosulay-cef.json");
        }
        let mut hashes = BTreeMap::new();
        for file in files(output)? {
            let relative = file
                .strip_prefix(output)?
                .to_str()
                .context("package path is not valid UTF-8")?;
            let name = if cfg!(windows) {
                relative.replace('\\', "/")
            } else {
                relative.to_owned()
            };
            hashes.insert(name, crate::sha256(&file)?);
        }
        let manifest = Manifest {
            product_version: PRODUCT_VERSION,
            cef_version: &receipt.cef_version,
            platform,
            archive: &receipt.archive,
            archive_sha256: &receipt.archive_sha256,
            runtime_sha256: hashes,
        };
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        fs::write(manifest_path, bytes)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    result
}

/// Write a ZIP32 package containing every regular file below `root` under `vosulay/`.
pub fn write_zip(output: &Path, root: &Path) -> Result<()> {
    let files = files(root)?;
    zip::write_zip(output, root, &files)
}

fn check_input(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading package input {}", path.display()))?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        bail!(
            "package input must be a regular file or directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("required package file missing: {}", source.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "package source must be a regular file: {}",
            source.display()
        );
    }
    if destination.exists() {
        bail!(
            "duplicate package basename: {}",
            destination.file_name().unwrap().to_string_lossy()
        );
    }
    fs::copy(source, destination).with_context(|| format!("copying {}", source.display()))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("reading package input {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("package input contains symlink: {}", source.display());
    }
    if destination.exists() {
        bail!(
            "duplicate package basename: {}",
            destination.file_name().unwrap().to_string_lossy()
        );
    }
    if metadata.is_file() {
        return copy_file(source, destination);
    }
    if !metadata.is_dir() {
        bail!("package input is not regular: {}", source.display());
    }
    fs::create_dir(destination).with_context(|| format!("creating {}", destination.display()))?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn strip_symbols(root: &Path) -> Result<()> {
    use std::io::Read;
    for file in files(root)? {
        let mut magic = [0_u8; 4];
        let is_elf = fs::File::open(&file)
            .and_then(|mut handle| handle.read_exact(&mut magic))
            .is_ok()
            && magic == *b"\x7fELF";
        if !is_elf {
            continue;
        }
        let status = std::process::Command::new("strip")
            .arg("--strip-unneeded")
            .arg(&file)
            .status()
            .with_context(|| format!("running strip on {}", file.display()))?;
        if !status.success() {
            bail!("strip failed for {}", file.display());
        }
    }
    Ok(())
}
