use std::collections::HashSet;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const CONFIG_VERSION: u32 = 1;
// CEF represents the OSR interval as integer microseconds; higher rates yield zero.
pub const MAX_FPS_LIMIT: u32 = 1_000_000;
pub const DEFAULT_ALLOWED_IPS: &str = "127.0.0.1,localhost,absolute";

pub const DEFAULT_REMOTE_LISTEN_HOST: &str = "127.0.0.1";
pub const DEFAULT_REMOTE_LISTEN_PORT: u16 = 5050;
pub const DEFAULT_DEVTOOLS_PORT: u16 = 39229;
pub const DEFAULT_VOSU_URL: &str = "http://127.0.0.1:24050/";

const DEFAULT_WIDTH: f64 = 500.0;
const DEFAULT_HEIGHT: f64 = 300.0;
const MIN_SIZE: f64 = 10.0;
const DEFAULT_OPACITY: f64 = 1.0;
const DEFAULT_ROTATION: f64 = 0.0;

const DEFAULT_PROFILE_HOTKEYS: [&str; 10] = [
    "Ctrl+0", "Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7", "Ctrl+8",
    "Ctrl+9",
];
const DEFAULT_COPY_OVERLAY_HOTKEY: &str = "Ctrl+C";
const DEFAULT_PASTE_OVERLAY_HOTKEY: &str = "Ctrl+V";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AppConfig {
    pub version: u32,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tosu_migration_completed: bool,
    #[serde(default)]
    pub settings: AppSettings,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub always_keep_overlays_in_view: bool,
    pub exit_edit_mode_on_focus_loss: bool,
    pub fps_limit: u32,
    pub allow_unrestricted_shared_array_buffer: bool,
    pub devtools_enabled: bool,
    #[serde(deserialize_with = "deserialize_devtools_port")]
    pub devtools_port: u16,
    pub allowed_ips: String,
    /// Serves the full editor at `manage.localhost:<port>/overlay` to this machine only.
    pub allow_local_profile_editing: bool,

    pub remote_listen_host: String,
    pub remote_listen_port: u16,
    /// vosu or tosu-compatible backend; `--vosu-url` overrides it for one run.
    pub vosu_url: String,
    pub hotkeys: HotkeySettings,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct HotkeySettings {
    pub profile_0: String,
    pub profile_1: String,
    pub profile_2: String,
    pub profile_3: String,
    pub profile_4: String,
    pub profile_5: String,
    pub profile_6: String,
    pub profile_7: String,
    pub profile_8: String,
    pub profile_9: String,
    pub copy_overlay: String,
    pub paste_overlay: String,
    pub undo: String,
    pub redo: String,
    pub refresh_selected_overlay: String,
    pub refresh_all_overlays: String,
    pub toggle_interaction: String,
    pub toggle_edit: String,
    pub toggle_visibility: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub overlays: Vec<Placement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruleset: Option<crate::ruleset::Ruleset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Placement {
    pub counter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<url::Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// vosu overlay settings profile this placement is pinned to; `None` follows
    /// vosu's active profile. Only installed counters (no `url`) can be pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vosu_profile: Option<String>,
    pub top: f64,
    pub left: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default = "default_rotation")]
    pub rotation: f64,
    /// Counter pixels hidden from each side of the frame (before scaling). The
    /// placement's position is the top-left corner of the remaining region.
    #[serde(default)]
    pub crop_top: f64,
    #[serde(default)]
    pub crop_right: f64,
    #[serde(default)]
    pub crop_bottom: f64,
    #[serde(default)]
    pub crop_left: f64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub z_index: i32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_profile: "default".to_owned(),
            profiles: vec![Profile::default()],
            tosu_migration_completed: false,
            settings: AppSettings::default(),
        }
    }
}

fn deserialize_devtools_port<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u16, D::Error> {
    let port = u16::deserialize(deserializer)?;
    if port == 0 {
        return Err(serde::de::Error::custom(
            "DevTools port must be from 1 to 65535",
        ));
    }
    Ok(port)
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            always_keep_overlays_in_view: false,
            exit_edit_mode_on_focus_loss: true,
            fps_limit: 0,
            allow_unrestricted_shared_array_buffer: false,
            devtools_enabled: false,
            devtools_port: DEFAULT_DEVTOOLS_PORT,
            allowed_ips: DEFAULT_ALLOWED_IPS.to_owned(),
            allow_local_profile_editing: false,

            remote_listen_host: DEFAULT_REMOTE_LISTEN_HOST.to_owned(),
            remote_listen_port: DEFAULT_REMOTE_LISTEN_PORT,
            vosu_url: DEFAULT_VOSU_URL.to_owned(),
            hotkeys: HotkeySettings::default(),
        }
    }
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            profile_0: DEFAULT_PROFILE_HOTKEYS[0].to_owned(),
            profile_1: DEFAULT_PROFILE_HOTKEYS[1].to_owned(),
            profile_2: DEFAULT_PROFILE_HOTKEYS[2].to_owned(),
            profile_3: DEFAULT_PROFILE_HOTKEYS[3].to_owned(),
            profile_4: DEFAULT_PROFILE_HOTKEYS[4].to_owned(),
            profile_5: DEFAULT_PROFILE_HOTKEYS[5].to_owned(),
            profile_6: DEFAULT_PROFILE_HOTKEYS[6].to_owned(),
            profile_7: DEFAULT_PROFILE_HOTKEYS[7].to_owned(),
            profile_8: DEFAULT_PROFILE_HOTKEYS[8].to_owned(),
            profile_9: DEFAULT_PROFILE_HOTKEYS[9].to_owned(),
            copy_overlay: DEFAULT_COPY_OVERLAY_HOTKEY.to_owned(),
            paste_overlay: DEFAULT_PASTE_OVERLAY_HOTKEY.to_owned(),
            undo: DEFAULT_UNDO_HOTKEY.to_owned(),
            redo: DEFAULT_REDO_HOTKEY.to_owned(),
            refresh_selected_overlay: DEFAULT_REFRESH_SELECTED_OVERLAY_HOTKEY.to_owned(),
            refresh_all_overlays: DEFAULT_REFRESH_ALL_OVERLAYS_HOTKEY.to_owned(),
            toggle_interaction: DEFAULT_TOGGLE_INTERACTION_HOTKEY.to_owned(),
            toggle_edit: DEFAULT_TOGGLE_EDIT_HOTKEY.to_owned(),
            toggle_visibility: String::new(),
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            overlays: Vec::new(),
            ruleset: None,
        }
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            counter_id: String::new(),
            url: None,
            name: None,
            vosu_profile: None,
            top: 0.0,
            left: 0.0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            scale: 1.0,
            opacity: DEFAULT_OPACITY,
            rotation: DEFAULT_ROTATION,
            crop_top: 0.0,
            crop_right: 0.0,
            crop_bottom: 0.0,
            crop_left: 0.0,
            enabled: true,
            z_index: 0,
        }
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        load_from_path(&path)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        save_to_path(self, &path)
    }

    pub fn normalize(&mut self) {
        self.version = CONFIG_VERSION;
        self.settings.normalize();

        if self.profiles.is_empty() {
            self.profiles.push(Profile::default());
        }

        let mut used_ids = HashSet::with_capacity(self.profiles.len());
        for (index, profile) in self.profiles.iter_mut().enumerate() {
            if profile.id.is_empty() || !used_ids.insert(profile.id.clone()) {
                profile.id = unique_profile_id(index + 1, &used_ids);
                used_ids.insert(profile.id.clone());
            }
            if profile.name.trim().is_empty() {
                profile.name = format!("Profile {}", index + 1);
            }
            for overlay in &mut profile.overlays {
                overlay.normalize();
            }
        }

        if !self
            .profiles
            .iter()
            .any(|profile| profile.id == self.active_profile)
        {
            self.active_profile.clone_from(&self.profiles[0].id);
        }
    }
}

impl AppSettings {
    fn normalize(&mut self) {
        self.allowed_ips = self.allowed_ips.trim().to_owned();
        self.fps_limit = self.fps_limit.min(MAX_FPS_LIMIT);

        self.remote_listen_host = normalized_remote_listen_host(&self.remote_listen_host);
        self.remote_listen_port = normalized_remote_listen_port(self.remote_listen_port);
        self.vosu_url = parse_vosu_url(&self.vosu_url)
            .map_or_else(|_| DEFAULT_VOSU_URL.to_owned(), String::from);
        self.hotkeys.normalize();
    }

    /// The configured backend; normalization guarantees it parses.
    pub fn vosu_url(&self) -> url::Url {
        parse_vosu_url(&self.vosu_url)
            .unwrap_or_else(|_| url::Url::parse(DEFAULT_VOSU_URL).expect("default vosu URL"))
    }
}

impl HotkeySettings {
    fn normalize(&mut self) {
        normalize_hotkey(&mut self.profile_0, DEFAULT_PROFILE_HOTKEYS[0]);
        normalize_hotkey(&mut self.profile_1, DEFAULT_PROFILE_HOTKEYS[1]);
        normalize_hotkey(&mut self.profile_2, DEFAULT_PROFILE_HOTKEYS[2]);
        normalize_hotkey(&mut self.profile_3, DEFAULT_PROFILE_HOTKEYS[3]);
        normalize_hotkey(&mut self.profile_4, DEFAULT_PROFILE_HOTKEYS[4]);
        normalize_hotkey(&mut self.profile_5, DEFAULT_PROFILE_HOTKEYS[5]);
        normalize_hotkey(&mut self.profile_6, DEFAULT_PROFILE_HOTKEYS[6]);
        normalize_hotkey(&mut self.profile_7, DEFAULT_PROFILE_HOTKEYS[7]);
        normalize_hotkey(&mut self.profile_8, DEFAULT_PROFILE_HOTKEYS[8]);
        normalize_hotkey(&mut self.profile_9, DEFAULT_PROFILE_HOTKEYS[9]);
        normalize_hotkey(&mut self.copy_overlay, DEFAULT_COPY_OVERLAY_HOTKEY);
        normalize_hotkey(&mut self.paste_overlay, DEFAULT_PASTE_OVERLAY_HOTKEY);
        normalize_hotkey(&mut self.undo, DEFAULT_UNDO_HOTKEY);
        normalize_hotkey(&mut self.redo, DEFAULT_REDO_HOTKEY);
        normalize_hotkey(
            &mut self.refresh_selected_overlay,
            DEFAULT_REFRESH_SELECTED_OVERLAY_HOTKEY,
        );
        normalize_hotkey(
            &mut self.refresh_all_overlays,
            DEFAULT_REFRESH_ALL_OVERLAYS_HOTKEY,
        );
        normalize_hotkey(
            &mut self.toggle_interaction,
            DEFAULT_TOGGLE_INTERACTION_HOTKEY,
        );
        normalize_hotkey(&mut self.toggle_edit, DEFAULT_TOGGLE_EDIT_HOTKEY);
        self.toggle_visibility = self.toggle_visibility.trim().to_owned();
    }
}

impl Placement {
    fn normalize(&mut self) {
        self.url = self
            .url
            .take()
            .filter(|url| matches!(url.scheme(), "http" | "https" | "vosulay-overlay"));
        self.name = self
            .name
            .take()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let catalog = self.url.is_none();
        self.vosu_profile = self
            .vosu_profile
            .take()
            .filter(|profile| catalog && valid_vosu_profile(profile));
        self.top = finite_or(self.top, 0.0);
        self.left = finite_or(self.left, 0.0);
        self.width = finite_or(self.width, DEFAULT_WIDTH).max(MIN_SIZE);
        self.height = finite_or(self.height, DEFAULT_HEIGHT).max(MIN_SIZE);
        self.scale = finite_or(self.scale, 1.0).max(0.1);
        self.opacity = finite_or(self.opacity, DEFAULT_OPACITY).clamp(0.0, 1.0);
        self.rotation = finite_or(self.rotation, DEFAULT_ROTATION).clamp(-180.0, 180.0);
        // At least MIN_SIZE of the frame stays visible on each axis.
        self.crop_left = finite_or(self.crop_left, 0.0).clamp(0.0, self.width - MIN_SIZE);
        self.crop_right =
            finite_or(self.crop_right, 0.0).clamp(0.0, self.width - MIN_SIZE - self.crop_left);
        self.crop_top = finite_or(self.crop_top, 0.0).clamp(0.0, self.height - MIN_SIZE);
        self.crop_bottom =
            finite_or(self.crop_bottom, 0.0).clamp(0.0, self.height - MIN_SIZE - self.crop_top);
        self.z_index = self.z_index.clamp(0, 999);
    }
}

/// vosu's overlay settings profile names: 1–48 ASCII letters, digits, `_` or `-`.
pub fn valid_vosu_profile(name: &str) -> bool {
    (1..=48).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn config_path() -> Result<PathBuf> {
    let base_dirs =
        BaseDirs::new().context("could not determine the user configuration directory")?;
    Ok(base_dirs.config_dir().join("vosulay").join("config.json"))
}

pub fn load_from_path(path: &Path) -> Result<AppConfig> {
    let (mut config, is_new) = match fs::read(path) {
        Ok(bytes) => {
            let config: AppConfig = serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse config at {}", path.display()))?;
            if config.version != CONFIG_VERSION {
                bail!(
                    "unsupported config version {} at {}; expected {}",
                    config.version,
                    path.display(),
                    CONFIG_VERSION
                );
            }
            (config, false)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => (AppConfig::default(), true),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read config at {}", path.display()));
        }
    };
    config.normalize();
    if is_new {
        save_to_path(&config, path).context("failed to persist default configuration")?;
    }
    Ok(config)
}

pub fn save_to_path(config: &AppConfig, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("config path {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create config directory {}", parent.display()))?;

    let encoded = serde_json::to_vec_pretty(config).context("failed to serialize config")?;

    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary config in {}", parent.display()))?;
    temporary
        .write_all(&encoded)
        .with_context(|| format!("failed to write temporary config for {}", path.display()))?;
    temporary
        .write_all(b"\n")
        .with_context(|| format!("failed to finish temporary config for {}", path.display()))?;
    temporary
        .as_file_mut()
        .sync_all()
        .with_context(|| format!("failed to sync temporary config for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace config at {}", path.display()))?;

    // The file is flushed before atomic replacement on both platforms. Unix
    // additionally permits fsync of the parent directory; Windows does not.
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync config directory {}", parent.display()))?;
    Ok(())
}

fn default_opacity() -> f64 {
    DEFAULT_OPACITY
}

fn default_rotation() -> f64 {
    DEFAULT_ROTATION
}

fn default_enabled() -> bool {
    true
}

fn normalize_hotkey(value: &mut String, default: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        value.clear();
        value.push_str(default);
    } else if trimmed.len() != value.len() {
        *value = trimmed.to_owned();
    }
}

const DEFAULT_UNDO_HOTKEY: &str = "Ctrl+Z";
const DEFAULT_REDO_HOTKEY: &str = "Ctrl+Shift+Z";
const DEFAULT_REFRESH_SELECTED_OVERLAY_HOTKEY: &str = "Ctrl+R";
const DEFAULT_REFRESH_ALL_OVERLAYS_HOTKEY: &str = "Ctrl+Shift+R";
pub(crate) const DEFAULT_TOGGLE_INTERACTION_HOTKEY: &str = "Ctrl+I";
pub(crate) const DEFAULT_TOGGLE_EDIT_HOTKEY: &str = "Ctrl+Shift+Space";
fn normalized_remote_listen_host(value: &str) -> String {
    let trimmed = value.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);
    if unbracketed.parse::<IpAddr>().is_ok() {
        unbracketed.to_owned()
    } else {
        DEFAULT_REMOTE_LISTEN_HOST.to_owned()
    }
}

fn normalized_remote_listen_port(value: u16) -> u16 {
    if value == 0 {
        DEFAULT_REMOTE_LISTEN_PORT
    } else {
        value
    }
}

/// A vosu or tosu-compatible backend: an HTTP(S) origin. vosulay requests
/// absolute API paths (`/json/v2`, `/api/counters/...`, `/settings`) on that
/// origin, so a path prefix would be silently dropped; it is rejected instead.
pub fn parse_vosu_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value.trim()).context("invalid vosu URL")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "vosu URL must be an HTTP(S) address with only a host and port, such as http://127.0.0.1:24050/"
    );
    Ok(url)
}

fn is_false(value: &bool) -> bool {
    !value
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

fn unique_profile_id(start: usize, used_ids: &HashSet<String>) -> String {
    let mut suffix = start;
    loop {
        let candidate = format!("profile-{suffix}");
        if !used_ids.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_configs_gain_native_toggle_defaults() {
        let config: AppConfig = serde_json::from_str(
            r#"{"version":1,"active_profile":"default","profiles":[],"settings":{"hotkeys":{"toggle_interaction":"Alt+I"}}}"#,
        ).unwrap();
        assert_eq!(config.settings.hotkeys.toggle_edit, "Ctrl+Shift+Space");
        assert!(config.settings.hotkeys.toggle_visibility.is_empty());
        assert_eq!(config.settings.hotkeys.toggle_interaction, "Alt+I");
    }
    #[test]
    fn visibility_hotkey_is_optional_and_trimmed() {
        let mut hotkeys = HotkeySettings {
            toggle_edit: " ".into(),
            toggle_visibility: " Alt+H ".into(),
            ..Default::default()
        };
        hotkeys.normalize();
        assert_eq!(hotkeys.toggle_edit, "Ctrl+Shift+Space");
        assert_eq!(hotkeys.toggle_visibility, "Alt+H");
        hotkeys.toggle_visibility = " ".into();
        hotkeys.normalize();
        assert!(hotkeys.toggle_visibility.is_empty());
    }
    #[test]
    fn devtools_migration_roundtrip_and_port_validation() {
        let legacy: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.devtools_enabled);
        assert_eq!(legacy.devtools_port, DEFAULT_DEVTOOLS_PORT);
        let enabled = AppSettings {
            devtools_enabled: true,
            devtools_port: 39230,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&enabled).unwrap();
        assert_eq!(
            serde_json::from_str::<AppSettings>(&encoded).unwrap(),
            enabled
        );
        for value in ["0", "-1", "65536", "1.5", "\"9229\""] {
            assert!(
                serde_json::from_str::<AppSettings>(&format!("{{\"devtools_port\":{value}}}"))
                    .is_err()
            );
        }
    }

    #[test]
    fn default_has_one_active_profile() {
        let config = AppConfig::default();

        assert_eq!(config.active_profile, config.profiles[0].id);
    }

    #[test]
    fn normalize_repairs_profiles_and_geometry() {
        let mut config = AppConfig {
            version: 99,
            active_profile: "missing".to_owned(),
            profiles: vec![Profile {
                id: String::new(),
                name: String::new(),
                ruleset: None,
                overlays: vec![Placement {
                    counter_id: "counter".to_owned(),
                    url: None,
                    name: None,
                    vosu_profile: Some("not a profile".to_owned()),
                    top: f64::NAN,
                    left: f64::INFINITY,
                    width: -5.0,
                    height: f64::NEG_INFINITY,
                    scale: 0.0,
                    opacity: f64::NAN,
                    rotation: f64::INFINITY,
                    crop_top: 100.0,
                    crop_right: 50.0,
                    crop_bottom: 500.0,
                    crop_left: f64::NAN,
                    enabled: true,
                    z_index: 10_000,
                }],
            }],
            tosu_migration_completed: false,
            settings: AppSettings::default(),
        };

        config.normalize();
        let settings = config.settings.clone();

        assert_eq!(
            config,
            AppConfig {
                version: CONFIG_VERSION,
                active_profile: "profile-1".to_owned(),
                profiles: vec![Profile {
                    id: "profile-1".to_owned(),
                    name: "Profile 1".to_owned(),
                    ruleset: None,
                    overlays: vec![Placement {
                        counter_id: "counter".to_owned(),
                        url: None,
                        name: None,
                        vosu_profile: None,
                        top: 0.0,
                        left: 0.0,
                        width: 10.0,
                        height: DEFAULT_HEIGHT,
                        scale: 0.1,
                        opacity: DEFAULT_OPACITY,
                        rotation: DEFAULT_ROTATION,
                        crop_top: 100.0,
                        crop_right: 0.0,
                        crop_bottom: DEFAULT_HEIGHT - MIN_SIZE - 100.0,
                        crop_left: 0.0,
                        enabled: true,
                        z_index: 999,
                    }],
                }],
                tosu_migration_completed: false,
                settings,
            }
        );
    }

    #[test]
    fn normalize_preserves_duplicate_counter_placements_in_order_and_normalizes_each() {
        let first = Placement {
            counter_id: "counter".to_owned(),
            top: 10.0,
            width: -5.0,
            ..Placement::default()
        };
        let duplicate = Placement {
            counter_id: "counter".to_owned(),
            top: 20.0,
            width: f64::NAN,
            ..Placement::default()
        };
        let mut config = AppConfig {
            profiles: vec![
                Profile {
                    overlays: vec![first.clone(), duplicate],
                    ..Profile::default()
                },
                Profile {
                    id: "other".to_owned(),
                    name: "Other".to_owned(),
                    ruleset: None,
                    overlays: vec![first],
                },
            ],
            ..AppConfig::default()
        };

        config.normalize();

        let expected_first = Placement {
            counter_id: "counter".to_owned(),
            top: 10.0,
            width: 10.0,
            ..Placement::default()
        };
        let expected_duplicate = Placement {
            counter_id: "counter".to_owned(),
            top: 20.0,
            width: DEFAULT_WIDTH,
            ..Placement::default()
        };
        assert_eq!(
            config.profiles[0].overlays,
            vec![expected_first.clone(), expected_duplicate]
        );
        assert_eq!(config.profiles[1].overlays, vec![expected_first]);
    }

    #[test]
    fn save_and_load_roundtrip_normalized_config() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("nested/config.json");
        let mut expected = AppConfig {
            version: 99,
            active_profile: "missing".to_owned(),
            profiles: vec![Profile {
                id: String::new(),
                name: "  ".to_owned(),
                ruleset: None,
                overlays: vec![Placement {
                    counter_id: "escaped/&counter".to_owned(),
                    url: Some(
                        url::Url::parse("https://example.com/overlay.html")
                            .expect("custom overlay URL should parse"),
                    ),
                    name: Some("  Custom overlay  ".to_owned()),
                    vosu_profile: None,
                    top: 12.5,
                    left: 20.0,
                    width: 640.0,
                    height: 360.0,
                    scale: 1.25,
                    opacity: 0.75,
                    rotation: -45.0,
                    crop_top: 4.0,
                    crop_right: 8.0,
                    crop_bottom: 0.0,
                    crop_left: 16.5,
                    enabled: true,
                    z_index: 4,
                }],
            }],
            tosu_migration_completed: false,
            settings: AppSettings {
                always_keep_overlays_in_view: true,
                allow_unrestricted_shared_array_buffer: true,
                ..AppSettings::default()
            },
        };
        expected.normalize();

        save_to_path(&expected, &path).expect("config should save");
        let encoded = fs::read(&path).expect("saved config should be readable");
        let actual = load_from_path(&path).expect("saved config should load");

        assert!(encoded.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<AppConfig>(&encoded)
                .expect("saved config should contain valid JSON"),
            expected
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn legacy_json_defaults_opacity_rotation_and_crop() {
        let config: AppConfig = serde_json::from_str(
            r#"{
                "version": 1,
                "active_profile": "default",
                "profiles": [{
                    "id": "default",
                    "name": "Default",
                    "overlays": [{
                        "counter_id": "counter",
                        "top": 0.0,
                        "left": 0.0,
                        "width": 500.0,
                        "height": 300.0,
                        "scale": 1.0,
                        "z_index": 0
                    }]
                }]
            }"#,
        )
        .expect("version 1 config without transform fields should deserialize");

        let placement = &config.profiles[0].overlays[0];
        assert_eq!(placement.opacity, DEFAULT_OPACITY);
        assert_eq!(placement.rotation, DEFAULT_ROTATION);
        assert_eq!(
            [
                placement.crop_top,
                placement.crop_right,
                placement.crop_bottom,
                placement.crop_left
            ],
            [0.0; 4]
        );
        assert!(placement.enabled);
        assert_eq!(placement.url, None);
        assert_eq!(placement.name, None);
    }

    #[test]
    fn disabled_placements_survive_roundtrip_with_order_and_geometry() {
        let mut config = AppConfig::default();
        config.profiles[0].overlays = vec![
            Placement {
                counter_id: "first".to_owned(),
                ..Placement::default()
            },
            Placement {
                counter_id: "disabled".to_owned(),
                enabled: false,
                left: 12.5,
                top: -3.0,
                width: 640.0,
                height: 360.0,
                scale: 1.25,
                opacity: 0.75,
                rotation: -15.0,
                z_index: 7,
                ..Placement::default()
            },
            Placement {
                counter_id: "last".to_owned(),
                ..Placement::default()
            },
        ];
        let encoded = serde_json::to_vec(&config).expect("serialize");
        let decoded = serde_json::from_slice::<AppConfig>(&encoded).expect("deserialize");
        assert_eq!(decoded.profiles[0].overlays, config.profiles[0].overlays);
    }

    #[test]
    fn old_json_receives_settings_defaults() {
        let config: AppConfig =
            serde_json::from_str(r#"{"version":1,"active_profile":"default","profiles":[]}"#)
                .expect("version 1 config without settings should deserialize");

        assert!(!config.settings.allow_unrestricted_shared_array_buffer);

        assert_eq!(config.settings, AppSettings::default());
    }
    #[test]
    fn persisted_fps_limits_stay_within_cefs_positive_interval_range() {
        for (requested, expected) in [
            (0, 0),
            (144, 144),
            (MAX_FPS_LIMIT, MAX_FPS_LIMIT),
            (u32::MAX, MAX_FPS_LIMIT),
        ] {
            let mut config = AppConfig::default();
            config.settings.fps_limit = requested;
            config.normalize();
            assert_eq!(config.settings.fps_limit, expected);
        }
    }

    #[test]
    fn partial_settings_receive_per_field_defaults() {
        let config: AppConfig = serde_json::from_str(
            r#"{
                "version": 1,
                "active_profile": "default",
                "profiles": [],
                "settings": {
                    "hotkeys": {"profile_0": "Alt+1"}
                }
            }"#,
        )
        .expect("partially specified settings should deserialize");
        let mut expected = AppSettings::default();
        expected.hotkeys.profile_0 = "Alt+1".to_owned();

        assert_eq!(config.settings, expected);
    }

    #[test]
    fn allow_unrestricted_shared_array_buffer_setting_survives_serde_roundtrip() {
        let mut config = AppConfig::default();
        config.settings.allow_unrestricted_shared_array_buffer = true;

        let encoded = serde_json::to_vec(&config).expect("config should serialize");
        let decoded = serde_json::from_slice::<AppConfig>(&encoded)
            .expect("encoded config should deserialize");

        assert_eq!(decoded.settings, config.settings);
    }

    #[test]
    fn allowed_ips_setting_survives_serde_roundtrip() {
        let mut config = AppConfig::default();
        config.settings.allowed_ips = "192.168.1.*, 2001:db8::1".to_owned();

        let encoded = serde_json::to_vec(&config).expect("config should serialize");
        let decoded = serde_json::from_slice::<AppConfig>(&encoded)
            .expect("encoded config should deserialize");

        assert_eq!(decoded.settings.allowed_ips, config.settings.allowed_ips);
    }

    #[test]
    fn removed_remote_host_is_ignored_and_new_hotkeys_roundtrip() {
        let config: AppConfig = serde_json::from_str(
            r#"{"version":1,"active_profile":"default","profiles":[],"settings":{
                "remote_access_host":"old.example",
                "hotkeys":{"undo":"Alt+Z","redo":"Alt+Shift+Z",
                    "refresh_selected_overlay":"Alt+R","refresh_all_overlays":"Alt+Shift+R"}
            }}"#,
        )
        .expect("old config should deserialize");
        let encoded = serde_json::to_value(&config).expect("config should serialize");
        assert!(encoded["settings"].get("remote_access_host").is_none());
        assert_eq!(config.settings.hotkeys.undo, "Alt+Z");
        assert_eq!(config.settings.hotkeys.redo, "Alt+Shift+Z");
        assert_eq!(config.settings.hotkeys.refresh_selected_overlay, "Alt+R");
        assert_eq!(config.settings.hotkeys.refresh_all_overlays, "Alt+Shift+R");
    }

    #[test]
    fn configs_without_focus_loss_setting_keep_leaving_edit_mode() {
        let legacy: AppConfig = serde_json::from_str(
            r#"{"version":1,"active_profile":"default","profiles":[],"settings":{}}"#,
        )
        .expect("old config should deserialize");
        assert!(legacy.settings.exit_edit_mode_on_focus_loss);

        let kept: AppConfig = serde_json::from_str(
            r#"{"version":1,"active_profile":"default","profiles":[],"settings":{"exit_edit_mode_on_focus_loss":false}}"#,
        )
        .expect("config should deserialize");
        assert!(!kept.settings.exit_edit_mode_on_focus_loss);
    }

    #[test]
    fn normalize_validates_vosu_url_as_an_origin() {
        let mut config = AppConfig::default();
        assert_eq!(
            config.settings.vosu_url(),
            url::Url::parse(DEFAULT_VOSU_URL).unwrap()
        );
        config.settings.vosu_url = " https://192.168.1.20:24050 ".to_owned();
        config.normalize();
        assert_eq!(config.settings.vosu_url, "https://192.168.1.20:24050/");
        // API paths are absolute, so a prefix would be silently dropped.
        for invalid in [
            "http://host/vosu",
            "ftp://host/",
            "http://user:pass@host/",
            "http://host/?x=1",
            "not a url",
            "",
        ] {
            config.settings.vosu_url = invalid.to_owned();
            config.normalize();
            assert_eq!(config.settings.vosu_url, DEFAULT_VOSU_URL, "{invalid:?}");
        }
    }

    #[test]
    fn normalize_defaults_remote_listen_host_and_port() {
        let mut config = AppConfig::default();
        assert_eq!(config.settings.remote_listen_host, "127.0.0.1");
        assert_eq!(config.settings.remote_listen_port, 5050);
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "127.0.0.1");
        assert_eq!(config.settings.remote_listen_port, 5050);
    }

    #[test]
    fn normalize_validates_remote_listen_host() {
        let mut config = AppConfig::default();
        config.settings.remote_listen_host = " 192.168.1.20 ".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "192.168.1.20");

        config.settings.remote_listen_host = "[::1]".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "::1");

        config.settings.remote_listen_host = "not-an-ip-address".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "127.0.0.1");

        config.settings.remote_listen_host = " \t ".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "127.0.0.1");
    }

    #[test]
    fn normalize_rejects_zero_remote_listen_port() {
        let mut config = AppConfig::default();
        config.settings.remote_listen_port = 0;
        config.normalize();
        assert_eq!(config.settings.remote_listen_port, 5050);

        config.settings.remote_listen_port = 8443;
        config.normalize();
        assert_eq!(config.settings.remote_listen_port, 8443);
    }

    #[test]
    fn remote_listen_settings_survive_serde_roundtrip() {
        let mut config = AppConfig::default();
        config.settings.remote_listen_host = "10.0.0.5".to_owned();
        config.settings.remote_listen_port = 9090;

        let encoded = serde_json::to_vec(&config).expect("config should serialize");
        let decoded = serde_json::from_slice::<AppConfig>(&encoded)
            .expect("encoded config should deserialize");

        assert_eq!(decoded.settings, config.settings);
    }

    #[test]
    fn normalize_preserves_explicitly_empty_allowed_ips() {
        let mut config = AppConfig::default();
        config.settings.allowed_ips = " \t\n".to_owned();

        config.normalize();

        assert_eq!(config.settings.allowed_ips, "");
        let encoded = serde_json::to_vec(&config).expect("config should serialize");
        let decoded = serde_json::from_slice::<AppConfig>(&encoded)
            .expect("encoded config should deserialize");
        assert_eq!(decoded.settings.allowed_ips, "");
    }

    #[test]
    fn legacy_right_click_setting_is_ignored_and_omitted() {
        for legacy_value in [false, true] {
            let config: AppConfig = serde_json::from_value(serde_json::json!({
                "version": 1,
                "active_profile": "default",
                "profiles": [],
                "settings": {
                    "right_click_overlay_settings_menu": legacy_value
                }
            }))
            .expect("legacy settings should deserialize");

            assert_eq!(config.settings, AppSettings::default());

            let encoded = serde_json::to_value(config).expect("config should serialize");
            assert!(
                encoded["settings"]
                    .get("right_click_overlay_settings_menu")
                    .is_none()
            );
        }
    }

    #[test]
    fn default_hotkeys_follow_profile_number_mapping() {
        let hotkeys = HotkeySettings::default();

        assert_eq!(
            [
                hotkeys.profile_0.as_str(),
                hotkeys.profile_1.as_str(),
                hotkeys.profile_2.as_str(),
                hotkeys.profile_3.as_str(),
                hotkeys.profile_4.as_str(),
                hotkeys.profile_5.as_str(),
                hotkeys.profile_6.as_str(),
                hotkeys.profile_7.as_str(),
                hotkeys.profile_8.as_str(),
                hotkeys.profile_9.as_str(),
                hotkeys.copy_overlay.as_str(),
                hotkeys.paste_overlay.as_str(),
            ],
            [
                "Ctrl+0", "Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7",
                "Ctrl+8", "Ctrl+9", "Ctrl+C", "Ctrl+V",
            ]
        );
    }

    #[test]
    fn normalize_blank_hotkeys_restores_each_action_default() {
        let mut hotkeys = HotkeySettings {
            profile_0: String::new(),
            profile_1: " ".to_owned(),
            profile_2: "\t".to_owned(),
            profile_3: "\n".to_owned(),
            profile_4: "  ".to_owned(),
            profile_5: "\t ".to_owned(),
            profile_6: " \n".to_owned(),
            profile_7: "\r".to_owned(),
            profile_8: "\r\n".to_owned(),
            profile_9: "\t\n".to_owned(),
            copy_overlay: " ".to_owned(),
            paste_overlay: "\t".to_owned(),
            undo: " ".to_owned(),
            redo: String::new(),
            refresh_selected_overlay: "\n".to_owned(),
            refresh_all_overlays: "\t".to_owned(),
            toggle_interaction: " ".to_owned(),
            toggle_edit: " ".to_owned(),
            toggle_visibility: " ".to_owned(),
        };

        hotkeys.normalize();

        assert_eq!(hotkeys, HotkeySettings::default());
    }

    #[test]
    fn normalize_trims_custom_hotkeys_without_rewriting_them() {
        let mut hotkeys = HotkeySettings {
            profile_0: "  Alt+Shift+K  ".to_owned(),
            copy_overlay: "\tMeta+Y\n".to_owned(),
            ..HotkeySettings::default()
        };

        hotkeys.normalize();

        assert_eq!(
            (hotkeys.profile_0.as_str(), hotkeys.copy_overlay.as_str()),
            ("Alt+Shift+K", "Meta+Y")
        );
    }

    #[test]
    fn custom_hotkeys_round_trip_without_rewriting() {
        let mut config = AppConfig::default();
        config.settings.hotkeys.profile_0 = "Alt+Shift+K".to_owned();
        config.settings.hotkeys.copy_overlay = "Meta+Y".to_owned();

        let encoded = serde_json::to_string(&config).expect("config should serialize");
        let decoded: AppConfig =
            serde_json::from_str(&encoded).expect("serialized config should deserialize");

        assert_eq!(decoded.settings.hotkeys, config.settings.hotkeys);
    }

    #[test]
    fn keep_overlays_in_view_setting_persists() {
        let mut config = AppConfig::default();
        config.settings.always_keep_overlays_in_view = true;

        let encoded = serde_json::to_string(&config).expect("config should serialize");
        let decoded: AppConfig =
            serde_json::from_str(&encoded).expect("serialized config should deserialize");

        assert_eq!(decoded.settings, config.settings);
    }

    #[test]
    fn normalize_clamps_opacity_and_rotation() {
        let mut below = Placement {
            opacity: -0.5,
            rotation: -270.0,
            ..Placement::default()
        };
        let mut above = Placement {
            opacity: 1.5,
            rotation: 270.0,
            ..Placement::default()
        };

        below.normalize();
        above.normalize();

        assert_eq!(below.opacity, 0.0);
        assert_eq!(below.rotation, -180.0);
        assert_eq!(above.opacity, 1.0);
        assert_eq!(above.rotation, 180.0);
    }

    #[test]
    fn vosu_profile_pin_survives_only_on_valid_catalog_placements() {
        let pinned = serde_json::json!({
            "counter_id": "accuracy",
            "vosu_profile": "stream_2-B",
            "top": 0, "left": 0, "width": 500, "height": 300, "scale": 1, "z_index": 0
        });
        let mut placement: Placement = serde_json::from_value(pinned.clone()).unwrap();
        placement.normalize();
        assert_eq!(placement.vosu_profile.as_deref(), Some("stream_2-B"));
        assert_eq!(
            serde_json::to_value(&placement).unwrap()["vosu_profile"],
            "stream_2-B"
        );

        for name in ["", "with space", "ü", &"x".repeat(49)] {
            let mut invalid = Placement {
                vosu_profile: Some(name.to_owned()),
                ..placement.clone()
            };
            invalid.normalize();
            assert_eq!(invalid.vosu_profile, None, "{name:?}");
        }
        assert!(valid_vosu_profile(&"x".repeat(48)));

        // A custom URL is not a vosu counter page, so a pin has nothing to select.
        let mut custom = Placement {
            url: Some(url::Url::parse("https://example.com/overlay.html").unwrap()),
            ..placement.clone()
        };
        custom.normalize();
        assert_eq!(custom.vosu_profile, None);

        let mut following = placement;
        following.vosu_profile = None;
        assert!(
            serde_json::to_value(&following)
                .unwrap()
                .get("vosu_profile")
                .is_none()
        );
    }

    #[test]
    fn normalize_custom_overlay_metadata() {
        let mut http = Placement {
            url: Some(
                url::Url::parse("http://example.com/overlay.html").expect("HTTP URL should parse"),
            ),
            name: Some("  Custom overlay  ".to_owned()),
            ..Placement::default()
        };
        let mut https = Placement {
            url: Some(
                url::Url::parse("https://example.com/overlay.html")
                    .expect("HTTPS URL should parse"),
            ),
            ..Placement::default()
        };
        let mut internal = Placement {
            url: Some(
                url::Url::parse("vosulay-overlay://0123456789abcdef/L3RtcC9vdmVybGF5/index.html")
                    .expect("internal overlay URL should parse"),
            ),
            ..Placement::default()
        };
        let mut file = Placement {
            url: Some(
                url::Url::parse("file:///tmp/custom-overlay.html").expect("file URL should parse"),
            ),
            ..Placement::default()
        };
        let mut ftp = Placement {
            url: Some(
                url::Url::parse("ftp://example.com/overlay.html")
                    .expect("unsupported URL should still parse"),
            ),
            name: Some("   ".to_owned()),
            ..Placement::default()
        };

        http.normalize();
        https.normalize();
        internal.normalize();
        file.normalize();
        ftp.normalize();

        assert_eq!(
            http.url.as_ref().map(url::Url::as_str),
            Some("http://example.com/overlay.html")
        );
        assert_eq!(http.name.as_deref(), Some("Custom overlay"));
        assert_eq!(
            https.url.as_ref().map(url::Url::as_str),
            Some("https://example.com/overlay.html")
        );
        assert_eq!(
            internal.url.as_ref().map(url::Url::as_str),
            Some("vosulay-overlay://0123456789abcdef/L3RtcC9vdmVybGF5/index.html")
        );
        assert_eq!(file.url, None);
        assert_eq!(ftp.url, None);
        assert_eq!(ftp.name, None);
    }

    #[test]
    fn load_normalizes_external_configuration() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            br#"{"version":1,"active_profile":"missing","profiles":[]}"#,
        )
        .expect("fixture should be written");

        let config = load_from_path(&path).expect("valid configuration should load");
        assert!(
            config
                .profiles
                .iter()
                .any(|profile| profile.id == config.active_profile),
            "normalization must select an existing profile"
        );
        assert_eq!(
            fs::read(&path).expect("original configuration should remain readable"),
            br#"{"version":1,"active_profile":"missing","profiles":[]}"#,
            "loading must not overwrite existing user configuration"
        );
    }
}
