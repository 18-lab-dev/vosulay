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
