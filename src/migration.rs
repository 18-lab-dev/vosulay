use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::config::{self, AppConfig, Placement, Profile};

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ImportedLayout {
    pub active_profile: String,
    pub profiles: Vec<Profile>,
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>> {
    v.as_object()
        .with_context(|| format!("{what} must be an object"))
}
fn string(v: Option<&Value>, what: &str) -> Result<String> {
    v.and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{what} must be a string"))
}
fn number(v: Option<&Value>, what: &str, default: f64) -> Result<f64> {
    match v {
        None => Ok(default),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .with_context(|| format!("{what} must be a finite number")),
    }
}
fn boolean(v: Option<&Value>, what: &str, default: bool) -> Result<bool> {
    match v {
        None => Ok(default),
        Some(v) => v
            .as_bool()
            .with_context(|| format!("{what} must be a boolean")),
    }
}
fn field<'a>(
    m: &'a serde_json::Map<String, Value>,
    first: &str,
    second: &str,
) -> Option<&'a Value> {
    m.get(first).or_else(|| m.get(second))
}

pub fn parse_tosu_layout(bytes: &[u8]) -> Result<ImportedLayout> {
    let root: Value = serde_json::from_slice(bytes).context("invalid tosu layout JSON")?;
    let root = object(&root, "layout root")?;
    if let Some(v) = root.get("obs_profile") {
        let _ = string(Some(v), "obs_profile")?;
    }
    let (profiles_value, active) = if let Some(p) = root.get("profiles") {
        (
            p,
            root.get("ingame_profile")
                .map(|v| string(Some(v), "ingame_profile"))
                .transpose()?,
        )
    } else if let Some(overlays) = root.get("overlays") {
        let mut profile = Profile::default();
        let placements = overlays.as_array().context("overlays must be an array")?;
        profile.overlays = placements
            .iter()
            .enumerate()
            .map(|(i, v)| parse_placement(v, 0, i))
            .collect::<Result<Vec<_>>>()?;
        return Ok(ImportedLayout {
            active_profile: profile.id.clone(),
            profiles: vec![profile],
        });
    } else if root.is_empty() {
        return Ok(ImportedLayout {
            active_profile: "default".into(),
            profiles: vec![Profile::default()],
        });
    } else {
        bail!("layout must contain profiles or overlays")
    };
    let values = profiles_value
        .as_array()
        .context("profiles must be an array")?;
    let profiles = values
        .iter()
        .enumerate()
        .map(|(i, p)| parse_profile(p, i))
        .collect::<Result<Vec<_>>>()?;
    let active_profile = active.unwrap_or_else(|| {
        profiles
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| "default".into())
    });
    Ok(ImportedLayout {
        active_profile,
        profiles,
    })
}

fn parse_profile(v: &Value, index: usize) -> Result<Profile> {
    let m = object(v, "profile")?;
    let id = match m.get("id") {
        Some(v) => string(Some(v), "profile.id")?,
        None => format!("imported-{index}"),
    };
    let name = match m.get("name") {
        Some(v) => string(Some(v), "profile.name")?,
        None => id.clone(),
    };
    let overlays = match m.get("overlays") {
        Some(v) => v.as_array().context("profile.overlays must be an array")?,
        None => bail!("profile.overlays is required"),
    };
    let overlays = overlays
        .iter()
        .enumerate()
        .map(|(i, v)| parse_placement(v, index, i))
        .collect::<Result<Vec<_>>>()?;
    Ok(Profile {
        id,
        name,
        overlays,
        ruleset: None,
    })
}
fn parse_placement(v: &Value, profile: usize, index: usize) -> Result<Placement> {
    let m = object(v, "overlay")?;
    let url_text = m
        .get("url")
        .map(|v| string(Some(v), "overlay.url"))
        .transpose()?;
    let custom_url = if let Some(raw) = url_text.as_ref().filter(|raw| !raw.starts_with("tosu://"))
    {
        let parsed = Url::parse(raw).with_context(|| format!("invalid overlay URL: {raw}"))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            bail!("custom overlay URL must be HTTP(S), have a host, and contain no credentials")
        }
        Some(parsed)
    } else {
        None
    };
    let folder = match m.get("folderName") {
        Some(v) => string(Some(v), "overlay.folderName")?,
        None if custom_url.is_some() => format!("custom-{profile}-{index}"),
        None => bail!("overlay.folderName is required for counter placements"),
    };
    if folder.trim().is_empty() {
        bail!("overlay.folderName must not be empty")
    }
    let url = if let Some(raw) = url_text.as_ref().filter(|raw| raw.starts_with("tosu://")) {
        let parsed = Url::parse(raw).context("invalid tosu counter URL")?;
        if parsed.host_str() != Some("server")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!("tosu URL must identify the server counter source")
        }
        let suffix = raw
            .strip_prefix("tosu://server/")
            .context("tosu URL must use the server counter source")?;
        if suffix.is_empty()
            || suffix
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || suffix != folder
        {
            bail!("tosu URL must identify the placement folderName")
        }
        None
    } else {
        custom_url
    };
    let mut p = Placement {
        counter_id: folder,
        url,
        name: m
            .get("name")
            .map(|v| string(Some(v), "overlay.name"))
            .transpose()?,
        ..Placement::default()
    };
    p.top = number(field(m, "top", "y"), "overlay.top", 0.0)?;
    p.left = number(field(m, "left", "x"), "overlay.left", 0.0)?;
    p.width = number(m.get("width"), "overlay.width", 500.0)?;
    p.height = number(m.get("height"), "overlay.height", 300.0)?;
    p.scale = number(m.get("scale"), "overlay.scale", 1.0)?;
    p.opacity = number(m.get("opacity"), "overlay.opacity", 1.0)?;
    p.rotation = number(m.get("rotation"), "overlay.rotation", 0.0)?;
    p.z_index = match m.get("z_index") {
        None => 0,
        Some(v) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .context("overlay.z_index must be an integer")?,
    };
    p.enabled = boolean(field(m, "enabled", "_enabled"), "overlay.enabled", true)?;
    Ok(p)
}

fn pristine(c: &AppConfig) -> bool {
    let mut candidate = c.clone();
    candidate.tosu_migration_completed = false;
    candidate == AppConfig::default()
}

pub fn import_automatically(config: &mut AppConfig, path: &Path, source: &Path) -> Result<bool> {
    if config.tosu_migration_completed || !pristine(config) {
        return Ok(false);
    }
    let mut file = match File::open(source) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("opening source {}", source.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("reading source metadata {}", source.display()))?;
    if !metadata.is_file() {
        bail!("tosu layout source is not a regular file")
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        bail!("tosu layout exceeds 1 MiB")
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        bail!("tosu layout exceeds 1 MiB")
    }
    let layout = parse_tosu_layout(&bytes)?;
    let persisted = config::load_from_path(path)
        .context("checking persisted native config before migration")?;
    if persisted != *config {
        return Ok(false);
    }
    let mut imported = config.clone();
    imported.profiles = layout.profiles;
    imported.active_profile = layout.active_profile;
    imported.tosu_migration_completed = true;
    imported.normalize();
    config::save_to_path(&imported, path)?;
    *config = imported;
    Ok(true)
}

pub fn append_layout(config: &mut AppConfig, layout: ImportedLayout) {
    let mut used: std::collections::HashSet<String> =
        config.profiles.iter().map(|p| p.id.clone()).collect();
    let active_name = layout.active_profile;
    let mut active = None;
    let mut first_imported = None;
    for mut p in layout.profiles {
        let original_id = p.id.clone();
        if used.contains(&p.id) || p.id.is_empty() {
            let base = format!(
                "{}-imported",
                if p.id.is_empty() { "profile" } else { &p.id }
            );
            let mut candidate = base.clone();
            let mut n = 2;
            while used.contains(&candidate) {
                candidate = format!("{base}-{n}");
                n += 1;
            }
            p.id = candidate;
        }
        used.insert(p.id.clone());
        if first_imported.is_none() {
            first_imported = Some(p.id.clone());
        }
        if active.is_none() && original_id == active_name {
            active = Some(p.id.clone());
        }
        config.profiles.push(p);
    }
    if let Some(id) = active.or(first_imported) {
        config.active_profile = id;
    }
    config.tosu_migration_completed = true;
    config.normalize();
}
