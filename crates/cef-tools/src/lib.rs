use anyhow::{Context, Result, bail, ensure};
use bzip2::read::BzDecoder;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub mod package;

pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PINS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/cef/pins.json");
const RECEIPT: &str = ".vosulay-cef-verified.json";
const MAX_MEMBERS: usize = 20_000;
const MAX_UNPACKED: u64 = 3_000_000_000;

#[derive(Deserialize)]
pub struct Pins {
    pub cef_version: String,
    pub platforms: BTreeMap<String, PlatformPin>,
    pub asdf_overlay: AsdfPin,
}

#[derive(Deserialize)]
pub struct PlatformPin {
    pub archive: String,
    pub url: String,
    pub size: u64,
    pub sha1: String,
    pub sha256: String,
    pub cef_path_subdir: String,
    pub required_files: Vec<String>,
    pub package_release_files: Vec<String>,
    pub package_resources: Vec<String>,
}

#[derive(Deserialize)]
pub struct AsdfPin {
    pub version: String,
    pub assets: Vec<AssetPin>,
}

#[derive(Deserialize)]
pub struct AssetPin {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Deserialize, Serialize)]
pub struct Receipt {
    pub platform: String,
    pub cef_version: String,
    pub archive: String,
    pub archive_sha256: String,
    pub product_version: String,
    pub runtime_sha256: BTreeMap<String, String>,
}

pub fn pins() -> Result<Pins> {
    serde_json::from_str(include_str!("../../../tools/cef/pins.json"))
        .context("invalid pinned CEF manifest")
}

fn platform_pin<'a>(pins: &'a Pins, platform: &str) -> Result<&'a PlatformPin> {
    pins.platforms
        .get(platform)
        .with_context(|| format!("unsupported CEF platform: {platform}"))
}

pub fn default_cache(kind: &str) -> Result<PathBuf> {
    let base = if let Some(path) = env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
        PathBuf::from(path)
    } else {
        PathBuf::from(
            env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .or_else(|| env::var_os("USERPROFILE").filter(|path| !path.is_empty()))
                .context("set XDG_CACHE_HOME or CEF_PATH; home directory is unavailable")?,
        )
        .join(".cache")
    };
    Ok(base.join("vosulay").join(kind))
}

fn regular_file(path: &Path) -> Result<File> {
    ensure!(
        fs::symlink_metadata(path)
            .with_context(|| format!("required regular file is missing: {}", path.display()))?
            .file_type()
            .is_file(),
        "file is not regular or is a symlink: {}",
        path.display()
    );
    File::open(path).with_context(|| format!("cannot open {}", path.display()))
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut file = regular_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Enumerate a regular directory tree without following symlinks or special nodes.
pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_files(root, &mut paths)?;
    paths.sort_unstable();
    Ok(paths)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    ensure!(
        fs::symlink_metadata(directory)?.file_type().is_dir(),
        "directory is not regular or is a symlink: {}",
        directory.display()
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            collect_files(&path, files)?;
        } else if kind.is_file() {
            files.push(path);
        } else {
            bail!(
                "symlink or special node in runtime tree: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn manifest(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for directory in ["Release", "Resources"] {
        for path in files(&root.join(directory))? {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 runtime path")?;
            // Backslashes are filename bytes on Unix, not path separators.
            let name = if cfg!(windows) {
                relative.replace('\\', "/")
            } else {
                relative.to_owned()
            };
            hashes.insert(name, sha256(&path)?);
        }
    }
    Ok(hashes)
}

fn verify_tree(root: &Path, pin: &PlatformPin) -> Result<()> {
    ensure!(
        root.file_name().and_then(|name| name.to_str()) == Some(pin.cef_path_subdir.as_str())
            && fs::symlink_metadata(root)?.file_type().is_dir(),
        "CEF_PATH directory name/type does not match pinned archive: {}",
        root.display()
    );
    let canonical_root = root.canonicalize()?;
    for relative in &pin.required_files {
        let path = root.join(relative);
        regular_file(&path)
            .with_context(|| format!("required CEF runtime file missing or unsafe: {relative}"))?;
        ensure!(
            path.canonicalize()?.starts_with(&canonical_root),
            "required CEF file escapes runtime tree: {relative}"
        );
    }
    Ok(())
}

pub fn verify_runtime(root: &Path, platform: &str) -> Result<Receipt> {
    let pins = pins()?;
    verify_runtime_with_pin(root, platform, &pins, platform_pin(&pins, platform)?)
}

fn verify_runtime_with_pin(
    root: &Path,
    platform: &str,
    pins: &Pins,
    pin: &PlatformPin,
) -> Result<Receipt> {
    verify_tree(root, pin)?;
    let receipt: Receipt = serde_json::from_reader(BufReader::new(
        regular_file(&root.join(RECEIPT)).context("verified CEF receipt is missing")?,
    ))
    .context("invalid verified CEF receipt")?;
    ensure!(
        receipt.platform == platform
            && receipt.cef_version == pins.cef_version
            && receipt.archive == pin.archive
            && receipt.archive_sha256 == pin.sha256
            && receipt.product_version == PRODUCT_VERSION,
        "verified CEF receipt does not match this platform/pin/product"
    );
    ensure!(
        receipt.runtime_sha256 == manifest(root)?,
        "CEF runtime tree digest mismatch"
    );
    Ok(receipt)
}

fn make_receipt(root: &Path, platform: &str, pins: &Pins, pin: &PlatformPin) -> Result<Receipt> {
    Ok(Receipt {
        platform: platform.to_owned(),
        cef_version: pins.cef_version.clone(),
        archive: pin.archive.clone(),
        archive_sha256: pin.sha256.clone(),
        product_version: PRODUCT_VERSION.to_owned(),
        runtime_sha256: manifest(root)?,
    })
}

fn cache_lock(cache: &Path) -> Result<File> {
    fs::create_dir_all(cache)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(".acquire.lock"))?;
    lock.lock().context("cannot lock CEF asset cache")?;
    Ok(lock)
}

struct Hashes {
    size: u64,
    sha1: Sha1,
    sha256: Sha256,
}

impl Hashes {
    fn new() -> Self {
        Self {
            size: 0,
            sha1: Sha1::new(),
            sha256: Sha256::new(),
        }
    }

    fn update(&mut self, data: &[u8], expected_size: u64, use_sha1: bool) -> Result<()> {
        self.size = self
            .size
            .checked_add(data.len() as u64)
            .context("asset size overflow")?;
        ensure!(
            self.size <= expected_size,
            "download exceeded pinned asset size"
        );
        if use_sha1 {
            self.sha1.update(data);
        }
        self.sha256.update(data);
        Ok(())
    }

    fn verify(self, size: u64, sha1: Option<&str>, sha256: &str) -> Result<()> {
        ensure!(self.size == size, "pinned asset size mismatch");
        if let Some(expected) = sha1 {
            ensure!(
                format!("{:x}", self.sha1.finalize()) == expected,
                "pinned asset SHA1 mismatch"
            );
        }
        ensure!(
            format!("{:x}", self.sha256.finalize()) == sha256,
            "pinned asset SHA256 mismatch"
        );
        Ok(())
    }
}

fn acquire_file(
    destination: &Path,
    url: &str,
    size: u64,
    sha1: Option<&str>,
    sha256: &str,
) -> Result<()> {
    let mut buffer = [0_u8; 65_536];
    let mut hashes = Hashes::new();
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            let mut file = regular_file(destination)?;
            ensure!(
                file.metadata()?.len() == size,
                "cached asset size mismatch: {}",
                destination.display()
            );
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hashes.update(&buffer[..count], size, sha1.is_some())?;
            }
            hashes.verify(size, sha1, sha256).with_context(|| {
                format!("cached asset integrity failure: {}", destination.display())
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("Downloading pinned runtime asset: {url}");
            let mut temporary = tempfile::Builder::new()
                .prefix(".asset-download-")
                .tempfile_in(destination.parent().context("asset path has no parent")?)?;
            let mut response = ureq::get(url)
                .config()
                .timeout_global(Some(Duration::from_secs(90)))
                .build()
                .call()
                .with_context(|| format!("cannot download pinned runtime asset: {url}"))?;
            let mut reader = response.body_mut().as_reader();
            loop {
                let count = reader.read(&mut buffer).with_context(|| {
                    format!(
                        "reading pinned asset {url} after {} of {size} bytes",
                        hashes.size
                    )
                })?;
                if count == 0 {
                    break;
                }
                hashes.update(&buffer[..count], size, sha1.is_some())?;
                temporary.write_all(&buffer[..count])?;
            }
            hashes.verify(size, sha1, sha256)?;
            temporary.flush()?;
            temporary.persist_noclobber(destination).with_context(|| {
                format!("cannot publish verified asset: {}", destination.display())
            })?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn safe_archive_path(name: &str, root_name: &str) -> Result<PathBuf> {
    ensure!(
        !name.starts_with('/')
            && !name.contains('\\')
            && !name.contains(':')
            && !name.split('/').any(|part| part == ".."),
        "unsafe archive path: {name}"
    );
    let mut parts = name
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".");
    ensure!(
        parts.next() == Some(root_name),
        "archive path is outside pinned root: {name}"
    );
    let mut path = PathBuf::from(root_name);
    for part in parts {
        path.push(part);
    }
    Ok(path)
}

fn extract_archive(archive: &Path, staging: &Path, pin: &PlatformPin) -> Result<()> {
    let decoder = BzDecoder::new(regular_file(archive)?);
    let mut archive = tar::Archive::new(decoder);
    let mut total = 0_u64;
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < MAX_MEMBERS, "archive member limit exceeded");
        let mut entry = entry?;
        let name = entry.path_bytes();
        let relative = safe_archive_path(std::str::from_utf8(&name)?, &pin.cef_path_subdir)?;
        let destination = staging.join(relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(destination)?;
        } else if kind.is_file() {
            let size = entry.header().size()?;
            total = total
                .checked_add(size)
                .context("archive unpacked size overflow")?;
            ensure!(
                total <= MAX_UNPACKED,
                "archive unpacked size limit exceeded"
            );
            fs::create_dir_all(
                destination
                    .parent()
                    .context("archive member has no parent")?,
            )?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            ensure!(
                io::copy(&mut entry, &mut output)? == size,
                "truncated archive member"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if entry.header().mode()? & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                };
                fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
            }
        } else {
            bail!("unsupported archive member type: {}", destination.display());
        }
    }
    Ok(())
}

pub fn acquire(platform: &str, cache: &Path) -> Result<PathBuf> {
    Ok(acquire_verified(platform, cache)?.0)
}

/// Return the receipt from the same verification pass, avoiding another full SDK hash in build.rs.
pub fn acquire_verified(platform: &str, cache: &Path) -> Result<(PathBuf, Receipt)> {
    let pins = pins()?;
    let pin = platform_pin(&pins, platform)?;
    let _lock = cache_lock(cache)?;
    let archive = cache.join(&pin.archive);
    acquire_file(&archive, &pin.url, pin.size, Some(&pin.sha1), &pin.sha256)?;
    let root = cache.join(&pin.cef_path_subdir);
    match fs::symlink_metadata(&root) {
        Ok(_) => {
            let receipt = verify_runtime_with_pin(&root, platform, &pins, pin)?;
            Ok((root.canonicalize()?, receipt))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let staging = tempfile::Builder::new()
                .prefix(".cef-extract-")
                .tempdir_in(cache)?;
            extract_archive(&archive, staging.path(), pin)?;
            let extracted = staging.path().join(&pin.cef_path_subdir);
            verify_tree(&extracted, pin)?;
            let receipt = make_receipt(&extracted, platform, &pins, pin)?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(extracted.join(RECEIPT))?;
            serde_json::to_writer_pretty(&mut file, &receipt)?;
            file.write_all(b"\n")?;
            file.flush()?;
            drop(file);
            fs::rename(extracted, &root).context("cannot publish verified CEF directory")?;
            Ok((root.canonicalize()?, receipt))
        }
        Err(error) => Err(error.into()),
    }
}

pub fn acquire_asdf(cache: &Path) -> Result<PathBuf> {
    let pins = pins()?;
    let _lock = cache_lock(cache)?;
    let root = cache.join(&pins.asdf_overlay.version);
    fs::create_dir_all(&root)?;
    ensure!(
        fs::symlink_metadata(&root)?.file_type().is_dir(),
        "unsafe native injection cache directory"
    );
    for pin in &pins.asdf_overlay.assets {
        acquire_file(&root.join(&pin.name), &pin.url, pin.size, None, &pin.sha256)?;
    }
    Ok(root.canonicalize()?)
}

#[cfg(test)]
mod tests;
