use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_ALLOWED_IPS: &str = "127.0.0.1,localhost,absolute";
pub const REMOTE_ACCESS_TOKEN_BYTES: usize = 32;
pub const DEFAULT_REMOTE_LISTEN_HOST: &str = "0.0.0.0";
pub const DEFAULT_REMOTE_LISTEN_PORT: u16 = 5050;

const DEFAULT_WIDTH: f64 = 500.0;
const DEFAULT_HEIGHT: f64 = 300.0;
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
    #[serde(default)]
    pub settings: AppSettings,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub always_keep_overlays_in_view: bool,
    pub allow_unrestricted_shared_array_buffer: bool,
    pub allowed_ips: String,
    pub remote_access_token: String,
    pub remote_access_host: String,
    pub remote_listen_host: String,
    pub remote_listen_port: u16,
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
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub overlays: Vec<Placement>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Placement {
    pub counter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<url::Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub top: f64,
    pub left: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default = "default_rotation")]
    pub rotation: f64,
    pub z_index: i32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_profile: "default".to_owned(),
            profiles: vec![Profile::default()],
            settings: AppSettings::default(),
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            always_keep_overlays_in_view: false,
            allow_unrestricted_shared_array_buffer: false,
            allowed_ips: DEFAULT_ALLOWED_IPS.to_owned(),
            remote_access_token: String::new(),
            remote_access_host: String::new(),
            remote_listen_host: DEFAULT_REMOTE_LISTEN_HOST.to_owned(),
            remote_listen_port: DEFAULT_REMOTE_LISTEN_PORT,
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
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            overlays: Vec::new(),
        }
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            counter_id: String::new(),
            url: None,
            name: None,
            top: 0.0,
            left: 0.0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            scale: 1.0,
            opacity: DEFAULT_OPACITY,
            rotation: DEFAULT_ROTATION,
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
        self.remote_access_token = normalized_remote_access_token(&self.remote_access_token);
        self.remote_access_host = normalized_remote_access_host(&self.remote_access_host);
        self.remote_listen_host = normalized_remote_listen_host(&self.remote_listen_host);
        self.remote_listen_port = normalized_remote_listen_port(self.remote_listen_port);
        self.hotkeys.normalize();
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
        self.top = finite_or(self.top, 0.0);
        self.left = finite_or(self.left, 0.0);
        self.width = finite_or(self.width, DEFAULT_WIDTH).max(10.0);
        self.height = finite_or(self.height, DEFAULT_HEIGHT).max(10.0);
        self.scale = finite_or(self.scale, 1.0).max(0.1);
        self.opacity = finite_or(self.opacity, DEFAULT_OPACITY).clamp(0.0, 1.0);
        self.rotation = finite_or(self.rotation, DEFAULT_ROTATION).clamp(-180.0, 180.0);
        self.z_index = self.z_index.clamp(0, 999);
    }
}

pub fn config_path() -> Result<PathBuf> {
    let base_dirs =
        BaseDirs::new().context("could not determine the user configuration directory")?;
    Ok(base_dirs.config_dir().join("vosulay").join("config.json"))
}

fn load_from_path(path: &Path) -> Result<AppConfig> {
    let (mut config, needs_token) = match fs::read(path) {
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
            let needs_token = !remote_access_token_is_valid(&config.settings.remote_access_token);
            (config, needs_token)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => (AppConfig::default(), true),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read config at {}", path.display()));
        }
    };
    config.normalize();
    if needs_token {
        save_to_path(&config, path).context("failed to persist remote access credential")?;
    }
    Ok(config)
}

fn save_to_path(config: &AppConfig, path: &Path) -> Result<()> {
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

fn normalize_hotkey(value: &mut String, default: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        value.clear();
        value.push_str(default);
    } else if trimmed.len() != value.len() {
        *value = trimmed.to_owned();
    }
}

fn remote_access_token_is_valid(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() == REMOTE_ACCESS_TOKEN_BYTES * 2
        && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn normalized_remote_access_token(value: &str) -> String {
    if remote_access_token_is_valid(value) {
        return value.trim().to_ascii_lowercase();
    }
    generate_remote_access_token()
}

fn generate_remote_access_token() -> String {
    let mut bytes = [0_u8; REMOTE_ACCESS_TOKEN_BYTES];
    getrandom::fill(&mut bytes).unwrap_or_else(|error| {
        panic!("operating system randomness unavailable for remote access token: {error}")
    });
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(REMOTE_ACCESS_TOKEN_BYTES * 2);
    for byte in bytes {
        token.push(HEX[usize::from(byte >> 4)] as char);
        token.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    token
}

fn normalized_remote_access_host(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return String::new();
    }
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);
    if let Ok(address) = unbracketed.parse::<IpAddr>() {
        return address.to_string();
    }
    url::Host::parse(trimmed)
        .map(|host| host.to_string().to_ascii_lowercase())
        .unwrap_or_default()
}

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
                overlays: vec![Placement {
                    counter_id: "counter".to_owned(),
                    url: None,
                    name: None,
                    top: f64::NAN,
                    left: f64::INFINITY,
                    width: -5.0,
                    height: f64::NEG_INFINITY,
                    scale: 0.0,
                    opacity: f64::NAN,
                    rotation: f64::INFINITY,
                    z_index: 10_000,
                }],
            }],
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
                    overlays: vec![Placement {
                        counter_id: "counter".to_owned(),
                        url: None,
                        name: None,
                        top: 0.0,
                        left: 0.0,
                        width: 10.0,
                        height: DEFAULT_HEIGHT,
                        scale: 0.1,
                        opacity: DEFAULT_OPACITY,
                        rotation: DEFAULT_ROTATION,
                        z_index: 999,
                    }],
                }],
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
                overlays: vec![Placement {
                    counter_id: "escaped/&counter".to_owned(),
                    url: Some(
                        url::Url::parse("https://example.com/overlay.html")
                            .expect("custom overlay URL should parse"),
                    ),
                    name: Some("  Custom overlay  ".to_owned()),
                    top: 12.5,
                    left: 20.0,
                    width: 640.0,
                    height: 360.0,
                    scale: 1.25,
                    opacity: 0.75,
                    rotation: -45.0,
                    z_index: 4,
                }],
            }],
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
    fn legacy_json_defaults_opacity_and_rotation() {
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
        assert_eq!(placement.url, None);
        assert_eq!(placement.name, None);
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
    fn normalize_generates_remote_access_token_when_missing() {
        let mut config = AppConfig::default();

        config.normalize();

        assert_eq!(
            config.settings.remote_access_token.len(),
            REMOTE_ACCESS_TOKEN_BYTES * 2
        );
        assert!(
            config
                .settings
                .remote_access_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }

    #[test]
    fn normalize_preserves_valid_remote_access_token() {
        let mut config = AppConfig::default();
        config.settings.remote_access_token = "AB".repeat(REMOTE_ACCESS_TOKEN_BYTES);

        config.normalize();

        assert_eq!(
            config.settings.remote_access_token,
            "ab".repeat(REMOTE_ACCESS_TOKEN_BYTES)
        );
    }

    #[test]
    fn normalize_validates_remote_access_host() {
        let mut config = AppConfig::default();
        config.settings.remote_access_host = " My-PC.Local. ".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_access_host, "my-pc.local");

        config.settings.remote_access_host = "[2001:db8::1]".to_owned();
        config.normalize();
        assert_eq!(config.settings.remote_access_host, "2001:db8::1");

        config.settings.remote_access_host = "https://attacker.example/path".to_owned();
        config.normalize();
        assert!(config.settings.remote_access_host.is_empty());
    }

    #[test]
    fn normalize_defaults_remote_listen_host_and_port() {
        let mut config = AppConfig::default();
        assert_eq!(config.settings.remote_listen_host, "0.0.0.0");
        assert_eq!(config.settings.remote_listen_port, 5050);
        config.normalize();
        assert_eq!(config.settings.remote_listen_host, "0.0.0.0");
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
        assert_eq!(config.settings.remote_listen_host, "0.0.0.0");
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
    fn load_normalizes_external_configuration_and_persists_credential() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            br#"{"version":1,"active_profile":"missing","profiles":[]}"#,
        )
        .expect("fixture should be written");

        let config = load_from_path(&path).expect("valid configuration should load");
        let persisted: AppConfig = serde_json::from_slice(
            &fs::read(&path).expect("migrated configuration should be readable"),
        )
        .expect("migrated configuration should be valid");

        assert_eq!(
            persisted.settings.remote_access_token,
            config.settings.remote_access_token
        );
    }

    #[test]
    fn save_reports_persistence_errors_with_context() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let parent = directory.path().join("not-a-directory");
        fs::write(&parent, b"blocking file").expect("fixture should be written");
        let path = parent.join("config.json");

        let error = save_to_path(&AppConfig::default(), &path)
            .expect_err("config directory creation must fail");

        assert!(
            error
                .to_string()
                .contains("failed to create config directory")
        );
    }

    #[test]
    fn load_reports_malformed_config() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.json");
        fs::write(&path, b"{not json").expect("fixture should be written");

        let error = load_from_path(&path).expect_err("malformed config must fail");

        assert!(error.to_string().contains("failed to parse config"));
    }
}
