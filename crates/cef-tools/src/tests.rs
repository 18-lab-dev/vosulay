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
