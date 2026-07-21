use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const CONFIG_VERSION: u32 = 1;

const DEFAULT_WIDTH: f64 = 500.0;
const DEFAULT_HEIGHT: f64 = 300.0;
const DEFAULT_OPACITY: f64 = 1.0;
const DEFAULT_ROTATION: f64 = 0.0;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AppConfig {
    pub version: u32,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
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
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(AppConfig::default()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read config at {}", path.display()));
        }
    };

    let mut config: AppConfig = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse config at {}", path.display()))?;
    if config.version != CONFIG_VERSION {
        bail!(
            "unsupported config version {} at {}; expected {}",
            config.version,
            path.display(),
            CONFIG_VERSION
        );
    }
    config.normalize();
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
        };

        config.normalize();

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
    fn load_normalizes_external_configuration() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            br#"{"version":1,"active_profile":"missing","profiles":[]}"#,
        )
        .expect("fixture should be written");

        let config = load_from_path(&path).expect("valid configuration should load");

        assert_eq!(config, AppConfig::default());
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
