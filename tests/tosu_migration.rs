use std::{fs, path::PathBuf};
use vosulay::{
    config::{self, AppConfig, Placement, Profile},
    migration::{ImportedLayout, append_layout, import_automatically, parse_tosu_layout},
};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/migration")
            .join(name),
    )
    .unwrap()
}
fn persisted_default(path: &std::path::Path) -> AppConfig {
    let config = AppConfig::default();
    config::save_to_path(&config, path).unwrap();
    config
}

#[test]
fn current_layout_preserves_selection_order_repeat_geometry_enabled_and_urls() {
    let layout = parse_tosu_layout(&fixture("tosu-4.26.2.json")).unwrap();
    assert_eq!(layout.active_profile, "game");
    assert_eq!(layout.profiles.len(), 2);
    let placements = &layout.profiles[0].overlays;
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].counter_id, "counter");
    assert_eq!(placements[1].counter_id, "counter");
    assert_eq!(placements[0].url, None);
    assert_eq!(
        placements[1].url.as_ref().unwrap().as_str(),
        "https://widgets.example.test/custom"
    );
    assert!(!placements[0].enabled);
    assert!(placements[1].enabled);
    assert_eq!((placements[0].opacity, placements[0].rotation), (0.4, 30.0));
    assert_eq!(
        (
            placements[0].top,
            placements[0].left,
            placements[0].width,
            placements[0].height,
            placements[0].scale,
            placements[0].z_index
        ),
        (10.0, 20.0, 640.0, 360.0, 1.25, 8)
    );
    assert_eq!(
        (
            placements[1].top,
            placements[1].left,
            placements[1].width,
            placements[1].height,
            placements[1].scale,
            placements[1].z_index
        ),
        (30.0, 40.0, 320.0, 180.0, 0.75, 9)
    );
    let second = &layout.profiles[1].overlays[0];
    assert_eq!(second.counter_id, "remote");
    assert_eq!(
        (
            second.top,
            second.left,
            second.width,
            second.height,
            second.scale,
            second.z_index
        ),
        (50.0, 60.0, 800.0, 200.0, 1.0, 2)
    );
}

#[test]
fn legacy_empty_and_custom_without_folder_shapes() {
    let legacy = parse_tosu_layout(&fixture("legacy.json")).unwrap();
    assert_eq!(legacy.profiles[0].id, "default");
    assert_eq!(legacy.profiles[0].name, "Default");
    assert_eq!(legacy.profiles[0].overlays.len(), 1);
    assert_eq!(
        parse_tosu_layout(b"{}").unwrap().profiles,
        vec![Profile::default()]
    );
    let custom = parse_tosu_layout(br#"{"profiles":[{"id":"p","name":"P","overlays":[{"url":"https://custom.example.test/widget"},{"url":"https://custom.example.test/widget"}]}]}"#).unwrap();
    assert_eq!(custom.profiles[0].overlays[0].counter_id, "custom-0-0");
    assert_eq!(custom.profiles[0].overlays[1].counter_id, "custom-0-1");
}

#[test]
fn rejects_malformed_values_and_unsafe_urls() {
    for input in [
        br#"{"profiles":{}}"#.as_slice(),
        br#"{"profiles":[{"id":"x","overlays":null}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{"folderName":"x","width":"20"}]}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{"folderName":"x","url":"file:///tmp/x"}]}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{}]}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{"folderName":"nested/id","url":"tosu://other/nested/id"}]}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{"folderName":"../x","url":"tosu://server/../x"}]}]}"#,
        br#"{"profiles":[{"id":"x","overlays":[{"folderName":"x","url":"https://user:pass@example.test/x"}]}]}"#,
    ] {
        assert!(parse_tosu_layout(input).is_err(), "accepted {}", String::from_utf8_lossy(input));
    }
    let nested = parse_tosu_layout(br#"{"profiles":[{"id":"x","overlays":[{"folderName":"nested/id","url":"tosu://server/nested/id"}]}]}"#).unwrap();
    assert_eq!(nested.profiles[0].overlays[0].counter_id, "nested/id");
    let spaced = parse_tosu_layout(
        br#"{"overlays":[{"folderName":"My Counter","url":"tosu://server/My Counter"}]}"#,
    )
    .unwrap();
    assert_eq!(spaced.profiles[0].overlays[0].counter_id, "My Counter");
}

#[test]
fn automatic_import_is_idempotent_and_source_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("config.json");
    let source = dir.path().join("tosu.json");
    let mut config = persisted_default(&native);
    fs::write(&source, fixture("tosu-4.26.2.json")).unwrap();
    let original = fs::read(&source).unwrap();
    assert!(import_automatically(&mut config, &native, &source).unwrap());
    assert_eq!(config.active_profile, "game");
    assert!(config.tosu_migration_completed);
    assert!(!import_automatically(&mut config, &native, &source).unwrap());
    assert_eq!(fs::read(source).unwrap(), original);
}

#[test]
fn missing_source_and_intervening_native_edit_do_not_import() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("config.json");
    let mut config = persisted_default(&native);
    assert!(!import_automatically(&mut config, &native, &dir.path().join("missing.json")).unwrap());
    let source = dir.path().join("tosu.json");
    fs::write(&source, fixture("tosu-4.26.2.json")).unwrap();
    let mut changed = config.clone();
    changed.settings.always_keep_overlays_in_view = true;
    config::save_to_path(&changed, &native).unwrap();
    assert!(!import_automatically(&mut config, &native, &source).unwrap());
    assert!(!config.tosu_migration_completed);
    assert!(
        config::load_from_path(&native)
            .unwrap()
            .settings
            .always_keep_overlays_in_view
    );
}

#[test]
fn existing_profile_or_settings_alone_prevent_automatic_import() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("tosu.json");
    fs::write(&source, fixture("tosu-4.26.2.json")).unwrap();
    for edit_profile in [true, false] {
        let native = dir.path().join(if edit_profile {
            "profile.json"
        } else {
            "settings.json"
        });
        let mut config = persisted_default(&native);
        if edit_profile {
            config.profiles[0].name = "User profile".into();
        } else {
            config.settings.always_keep_overlays_in_view = true;
        }
        config::save_to_path(&config, &native).unwrap();
        assert!(!import_automatically(&mut config, &native, &source).unwrap());
        let saved = config::load_from_path(&native).unwrap();
        assert_eq!(
            saved.profiles[0].name,
            if edit_profile {
                "User profile"
            } else {
                "Default"
            }
        );
        assert_eq!(saved.settings.always_keep_overlays_in_view, !edit_profile);
    }
}

#[test]
fn malformed_import_leaves_native_config_bytes_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let native = dir.path().join("config.json");
    let source = dir.path().join("tosu.json");
    let mut config = persisted_default(&native);
    fs::write(&source, br#"{"profiles":[{"id":"x","overlays":"bad"}]}"#).unwrap();
    let before = fs::read(&native).unwrap();
    assert!(import_automatically(&mut config, &native, &source).is_err());
    assert_eq!(fs::read(native).unwrap(), before);
    assert!(!config.tosu_migration_completed);
}

#[test]
fn manual_import_repairs_collisions_without_replacing_existing_profile() {
    let mut config = AppConfig::default();
    append_layout(
        &mut config,
        ImportedLayout {
            active_profile: "default".into(),
            profiles: vec![Profile {
                id: "default".into(),
                name: "Imported".into(),
                overlays: vec![Placement {
                    counter_id: "x".into(),
                    ..Placement::default()
                }],
            }],
        },
    );
    assert_eq!(config.profiles[0].name, "Default");
    assert_eq!(config.profiles[1].id, "default-imported");
    assert_eq!(config.active_profile, "default-imported");
    assert!(config.tosu_migration_completed);
}
