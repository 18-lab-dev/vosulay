use super::*;
use bzip2::{Compression, write::BzEncoder};

fn fixture_pin() -> PlatformPin {
    PlatformPin {
        archive: "sdk.tar.bz2".into(),
        url: String::new(),
        size: 0,
        sha1: String::new(),
        sha256: "fixture-digest".into(),
        cef_path_subdir: "sdk".into(),
        required_files: vec!["Release/library".into(), "Resources/data".into()],
        package_release_files: vec![],
        package_resources: vec![],
    }
}

fn archive(path: &Path, members: &[(&str, tar::EntryType, &[u8], u32)]) {
    let encoder = BzEncoder::new(File::create(path).unwrap(), Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for &(name, kind, data, mode) in members {
        let mut header = tar::Header::new_gnu();
        // Raw names deliberately permit traversal fixtures the tar builder normally refuses.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("target").unwrap();
        }
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

#[test]
fn extraction_accepts_regular_files_and_preserves_executable_intent() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("archive");
    archive(
        &input,
        &[
            (
                "./sdk/Release/run",
                tar::EntryType::Regular,
                b"executable",
                0o4755,
            ),
            (
                "sdk/Resources/data",
                tar::EntryType::Regular,
                b"resource",
                0o666,
            ),
        ],
    );
    let output = temp.path().join("output");
    fs::create_dir(&output).unwrap();
    extract_archive(&input, &output, &fixture_pin()).unwrap();
    assert_eq!(
        fs::read(output.join("sdk/Release/run")).unwrap(),
        b"executable"
    );
    assert_eq!(
        fs::read(output.join("sdk/Resources/data")).unwrap(),
        b"resource"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(output.join("sdk/Release/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
        assert_eq!(
            fs::metadata(output.join("sdk/Resources/data"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
    }
}

#[test]
fn extraction_rejects_paths_outside_the_pinned_root() {
    for name in [
        "../escaped",
        "/sdk/escaped",
        "other/escaped",
        "sdk/../escaped",
        "sdk\\escaped",
        "sdk/C:escaped",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("archive");
        archive(
            &input,
            &[(name, tar::EntryType::Regular, b"payload", 0o644)],
        );
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        assert!(
            extract_archive(&input, &output, &fixture_pin()).is_err(),
            "accepted {name}"
        );
        assert!(!temp.path().join("escaped").exists());
    }
}

#[test]
fn extraction_rejects_links_and_special_nodes() {
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::Char,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("archive");
        archive(&input, &[("sdk/unsafe", kind, b"", 0o644)]);
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        assert!(extract_archive(&input, &output, &fixture_pin()).is_err());
        assert!(!output.join("sdk/unsafe").exists());
    }
}

#[test]
fn extraction_skips_github_pax_global_header() {
    // codeload.github.com source archives (the pinned asdf-overlay source) start
    // with a global header named outside the root that carries the commit.
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("archive");
    archive(
        &input,
        &[
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                b"52 comment=c925244c8d14ca36d82585834cdfebf13d6ce86b\n",
                0o666,
            ),
            (
                "sdk/Cargo.toml",
                tar::EntryType::Regular,
                b"[workspace]",
                0o644,
            ),
        ],
    );
    let output = temp.path().join("output");
    fs::create_dir(&output).unwrap();
    extract_archive(&input, &output, &fixture_pin()).unwrap();
    assert_eq!(
        fs::read(output.join("sdk/Cargo.toml")).unwrap(),
        b"[workspace]"
    );
    assert!(!output.join("pax_global_header").exists());
}

#[test]
fn extraction_does_not_overwrite_duplicate_files() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("archive");
    archive(
        &input,
        &[
            ("sdk/file", tar::EntryType::Regular, b"original", 0o644),
            ("sdk/file", tar::EntryType::Regular, b"replacement", 0o644),
        ],
    );
    let output = temp.path().join("output");
    fs::create_dir(&output).unwrap();
    assert!(extract_archive(&input, &output, &fixture_pin()).is_err());
    assert_eq!(fs::read(output.join("sdk/file")).unwrap(), b"original");
}

#[test]
fn extraction_bounds_declared_unpacked_bytes_before_writing() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("archive");
    let mut header = tar::Header::new_gnu();
    header.set_path("sdk/oversized").unwrap();
    header.set_size(MAX_UNPACKED + 1);
    header.set_mode(0o644);
    header.set_cksum();
    let mut encoder = BzEncoder::new(File::create(&input).unwrap(), Compression::fast());
    encoder.write_all(header.as_bytes()).unwrap();
    encoder.finish().unwrap();
    let output = temp.path().join("output");
    fs::create_dir(&output).unwrap();
    assert!(extract_archive(&input, &output, &fixture_pin()).is_err());
    assert!(!output.join("sdk/oversized").exists());
}

#[test]
fn verified_receipts_reject_changed_added_and_missing_runtime_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sdk");
    fs::create_dir_all(root.join("Release")).unwrap();
    fs::create_dir_all(root.join("Resources")).unwrap();
    fs::write(root.join("Release/library"), b"library").unwrap();
    fs::write(root.join("Resources/data"), b"resource").unwrap();
    fs::create_dir_all(root.join("Release/nested")).unwrap();
    fs::write(root.join("Release/nested/item"), b"library").unwrap();
    let pins = pins().unwrap();
    let pin = fixture_pin();
    let receipt = make_receipt(&root, "linux-x86_64", &pins, &pin).unwrap();
    serde_json::to_writer(File::create(root.join(RECEIPT)).unwrap(), &receipt).unwrap();
    verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).unwrap();
    fs::write(root.join("Resources/data"), b"tampered").unwrap();
    assert!(verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).is_err());
    fs::write(root.join("Resources/data"), b"resource").unwrap();
    fs::write(root.join("Resources/extra"), b"extra").unwrap();
    assert!(verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).is_err());
    fs::remove_file(root.join("Resources/extra")).unwrap();
    #[cfg(unix)]
    {
        let alias = root.join(r"Release/nested\item");
        fs::write(&alias, b"library").unwrap();
        assert!(verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).is_err());
        fs::remove_file(alias).unwrap();
    }
    fs::remove_file(root.join("Resources/data")).unwrap();
    assert!(verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).is_err());
}

#[test]
fn cached_sdk_receipt_from_another_vosulay_version_still_verifies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sdk");
    fs::create_dir_all(root.join("Release")).unwrap();
    fs::create_dir_all(root.join("Resources")).unwrap();
    fs::write(root.join("Release/library"), b"library").unwrap();
    fs::write(root.join("Resources/data"), b"resource").unwrap();
    let pins = pins().unwrap();
    let pin = fixture_pin();
    // Receipts used to record the vosulay version, so a version bump rejected
    // an intact cached SDK and every build failed until the cache was cleared.
    let mut receipt =
        serde_json::to_value(make_receipt(&root, "linux-x86_64", &pins, &pin).unwrap()).unwrap();
    receipt["product_version"] = serde_json::json!("0.0.1");
    serde_json::to_writer(File::create(root.join(RECEIPT)).unwrap(), &receipt).unwrap();
    verify_runtime_with_pin(&root, "linux-x86_64", &pins, &pin).unwrap();
}

#[test]
fn dll_version_resource_is_a_res_file_with_signing_metadata() {
    let resource = package::windows_dll_version_resource("vosulay.dll").unwrap();
    let u32_at =
        |offset: usize| u32::from_le_bytes(resource[offset..offset + 4].try_into().unwrap());
    // The format-identifying empty entry, then RT_VERSION #1.
    assert_eq!((u32_at(0), u32_at(4)), (0, 32));
    assert_eq!(
        &resource[32 + 8..32 + 16],
        &[0xff, 0xff, 16, 0, 0xff, 0xff, 1, 0]
    );
    let size = u32_at(32) as usize;
    assert_eq!(resource.len(), 64 + size.next_multiple_of(4));

    let version = editpe::VersionInfo::parse(&resource[64..64 + size]).unwrap();
    let strings = &version.strings[0].strings;
    for (key, value) in [
        ("ProductName", "Vosulay"),
        ("ProductVersion", PRODUCT_VERSION),
        ("FileVersion", PRODUCT_VERSION),
        ("OriginalFilename", "vosulay.dll"),
    ] {
        assert_eq!(strings.get(key).map(String::as_str), Some(value), "{key}");
    }
    assert_eq!(version.info.file_type, 2, "VFT_DLL");
}

/// How the local server answers one request for the asset.
#[derive(Clone, Copy)]
enum Reply {
    /// 200 for the whole asset, closing after `cut` body bytes.
    Full { cut: usize },
    /// Honors `Range` with a 206, closing after `cut` body bytes.
    Ranged { cut: usize },
    /// A 206 claiming the range starts at `start`, whatever was asked.
    WrongRange { start: usize },
}

/// Serves `replies` in order, one per connection, and returns the `Range`
/// header of each request.
fn cut_off_server(
    asset: Vec<u8>,
    replies: Vec<Reply>,
) -> (String, std::thread::JoinHandle<Vec<Option<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/asset", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut ranges = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            let range = request
                .lines()
                .find_map(|line| {
                    line.strip_prefix("range: ")
                        .or_else(|| line.strip_prefix("Range: "))
                })
                .map(str::to_owned);
            let requested = range
                .as_deref()
                .and_then(|range| range.strip_prefix("bytes="))
                .and_then(|range| range.strip_suffix('-'))
                .map(|start| start.parse::<usize>().unwrap());
            let (status, start, cut) = match (reply, requested) {
                (Reply::Full { cut }, _) => ("200 OK", 0, cut),
                (Reply::Ranged { cut }, Some(start)) => ("206 Partial Content", start, cut),
                (Reply::Ranged { cut }, None) => ("200 OK", 0, cut),
                (Reply::WrongRange { start }, _) => ("206 Partial Content", start, usize::MAX),
            };
            let body = &asset[start..];
            let mut head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if status.starts_with("206") {
                head += &format!(
                    "Content-Range: bytes {start}-{}/{}\r\n",
                    asset.len() - 1,
                    asset.len()
                );
            }
            head += "\r\n";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body[..cut.min(body.len())]);
            ranges.push(range);
        }
        ranges
    });
    (url, server)
}

fn asset_fixture() -> (Vec<u8>, String, String) {
    let asset: Vec<u8> = (0..300_000_u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let sha1 = format!("{:x}", Sha1::digest(&asset));
    let sha256 = format!("{:x}", Sha256::digest(&asset));
    (asset, sha1, sha256)
}

const RETRIES: Retries = Retries {
    attempts: 3,
    stalls: 2,
    delay: Duration::ZERO,
};

fn leftover_downloads(directory: &Path) -> usize {
    fs::read_dir(directory)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".asset-download-")
        })
        .count()
}

#[test]
fn interrupted_download_resumes_from_where_it_was_cut() {
    let (asset, sha1, sha256) = asset_fixture();
    let (url, server) = cut_off_server(
        asset.clone(),
        vec![
            Reply::Full { cut: 100_000 },
            Reply::Ranged { cut: 120_000 },
            Reply::Ranged { cut: usize::MAX },
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("asset.tar.bz2");
    download_verified(
        &destination,
        &url,
        asset.len() as u64,
        Some(&sha1),
        &sha256,
        RETRIES,
    )
    .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), asset);
    assert_eq!(
        server.join().unwrap(),
        [
            None,
            Some("bytes=100000-".into()),
            Some("bytes=220000-".into())
        ]
    );
    assert_eq!(leftover_downloads(directory.path()), 0);
}

#[test]
fn server_ignoring_the_range_restarts_the_download() {
    let (asset, sha1, sha256) = asset_fixture();
    let (url, server) = cut_off_server(
        asset.clone(),
        vec![
            Reply::Full { cut: 100_000 },
            Reply::Full { cut: usize::MAX },
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("asset.tar.bz2");
    download_verified(
        &destination,
        &url,
        asset.len() as u64,
        Some(&sha1),
        &sha256,
        RETRIES,
    )
    .unwrap();
    // The 200 body replaced the partial data rather than being appended to it.
    assert_eq!(fs::read(&destination).unwrap(), asset);
    assert_eq!(server.join().unwrap(), [None, Some("bytes=100000-".into())]);
}

#[test]
fn a_range_that_does_not_continue_the_file_is_discarded() {
    let (asset, sha1, sha256) = asset_fixture();
    let (url, server) = cut_off_server(
        asset.clone(),
        vec![
            Reply::Full { cut: 100_000 },
            Reply::WrongRange { start: 50_000 },
            Reply::Full { cut: usize::MAX },
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("asset.tar.bz2");
    download_verified(
        &destination,
        &url,
        asset.len() as u64,
        Some(&sha1),
        &sha256,
        RETRIES,
    )
    .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), asset);
    // After the mismatched 206 the download starts over without a range.
    assert_eq!(
        server.join().unwrap(),
        [None, Some("bytes=100000-".into()), None]
    );
}

#[test]
fn a_download_cut_on_every_attempt_fails_at_the_cap_without_publishing() {
    let (asset, sha1, sha256) = asset_fixture();
    let (url, server) = cut_off_server(
        asset.clone(),
        vec![
            Reply::Full { cut: 10_000 },
            Reply::Ranged { cut: 10_000 },
            Reply::Ranged { cut: 10_000 },
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("asset.tar.bz2");
    let error = download_verified(
        &destination,
        &url,
        asset.len() as u64,
        Some(&sha1),
        &sha256,
        RETRIES,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains(
            "after 30000 of 300000 bytes; gave up after 2 attempts without progress or 3 in all"
        ),
        "{error:#}"
    );
    assert_eq!(server.join().unwrap().len(), 3);
    assert!(!destination.exists());
    assert_eq!(leftover_downloads(directory.path()), 0);
}

#[test]
fn a_download_that_stops_progressing_gives_up_before_the_attempt_cap() {
    let (asset, sha1, sha256) = asset_fixture();
    // Progress, then two resumes that deliver nothing.
    let (url, server) = cut_off_server(
        asset.clone(),
        vec![
            Reply::Full { cut: 10_000 },
            Reply::Ranged { cut: 0 },
            Reply::Ranged { cut: 0 },
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("asset.tar.bz2");
    let retries = Retries {
        attempts: 10,
        stalls: 2,
        delay: Duration::ZERO,
    };
    let error = download_verified(
        &destination,
        &url,
        asset.len() as u64,
        Some(&sha1),
        &sha256,
        retries,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("after 10000 of 300000 bytes"),
        "{error:#}"
    );
    // Three requests, not ten: the stall limit ended it.
    assert_eq!(server.join().unwrap().len(), 3);
    assert!(!destination.exists());
    assert_eq!(leftover_downloads(directory.path()), 0);
}
