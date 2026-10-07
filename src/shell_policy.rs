use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayStatus {
    Ready,
    Invalid,
    Denied,
    NotFound,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOverlay {
    pub status: OverlayStatus,
    pub path: String,
}
#[derive(Default)]
pub struct OverlayAuthorization {
    roots: HashMap<String, PathBuf>,
}

pub fn overlay_url(path: &str, token: &str) -> Result<String> {
    if !valid_token(token) {
        bail!("invalid overlay capability token");
    }
    let target = fs::canonicalize(path).context("selected overlay does not exist")?;
    let metadata = fs::metadata(&target).context("could not inspect selected overlay")?;
    if !metadata.is_file() {
        bail!("selected overlay is not a regular file");
    }
    let root = target
        .parent()
        .context("selected overlay has no parent directory")?;
    let root_text = root.to_str().context("overlay root is not valid UTF-8")?;
    let filename = target
        .file_name()
        .and_then(|value| value.to_str())
        .context("overlay filename is not valid UTF-8")?;
    let root64 = URL_SAFE_NO_PAD.encode(root_text.as_bytes());
    let mut url =
        Url::parse(&format!("vosulay-overlay://{token}/")).expect("static capability URL is valid");
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("could not construct overlay URL"))?
        .push(&root64)
        .push(filename);
    Ok(url.into())
}

impl OverlayAuthorization {
    pub fn authorize(&mut self, url: &str) -> bool {
        self.authorize_batch(std::slice::from_ref(&url))
    }
    pub fn authorize_json(&mut self, urls_json: &str) -> bool {
        let Ok(urls) = serde_json::from_str::<Vec<String>>(urls_json) else {
            return false;
        };
        self.authorize_batch(&urls.iter().map(String::as_str).collect::<Vec<_>>())
    }
    fn authorize_batch(&mut self, urls: &[&str]) -> bool {
        let mut additions = HashMap::new();
        for url in urls {
            let Ok(capability) = parse_capability(url) else {
                return false;
            };
            if self
                .roots
                .get(&capability.token)
                .is_some_and(|root| root != &capability.root)
                || additions
                    .get(&capability.token)
                    .is_some_and(|root| root != &capability.root)
            {
                return false;
            }
            additions.insert(capability.token, capability.root);
        }
        self.roots.extend(additions);
        true
    }
    pub fn resolve(&self, url: &str) -> ResolvedOverlay {
        let capability = match parse_capability(url) {
            Ok(value) => value,
            Err(()) => return resolved(OverlayStatus::Invalid, None),
        };
        if self.roots.get(&capability.token) != Some(&capability.root) {
            return resolved(OverlayStatus::Denied, None);
        }
        let requested = capability.root.join(&capability.relative);
        let target = match fs::canonicalize(&requested) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return resolved(OverlayStatus::NotFound, None);
            }
            Err(_) => return resolved(OverlayStatus::Denied, None),
        };
        if !target.starts_with(&capability.root) {
            return resolved(OverlayStatus::Denied, None);
        }
        let Ok(metadata) = fs::metadata(&target) else {
            return resolved(OverlayStatus::Denied, None);
        };
        if !metadata.is_file() || !readable(&metadata) || fs::File::open(&target).is_err() {
            return resolved(OverlayStatus::Denied, None);
        }
        resolved(OverlayStatus::Ready, target.to_str())
    }
}

struct Capability {
    token: String,
    root: PathBuf,
    relative: PathBuf,
}
fn parse_capability(value: &str) -> std::result::Result<Capability, ()> {
    let url = Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "vosulay-overlay"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let token = url
        .host_str()
        .filter(|value| valid_token(value))
        .ok_or(())?
        .to_owned();
    let segments = url.path_segments().ok_or(())?.collect::<Vec<_>>();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return Err(());
    }
    let root_bytes = URL_SAFE_NO_PAD.decode(segments[0]).map_err(|_| ())?;
    if URL_SAFE_NO_PAD.encode(&root_bytes) != segments[0] {
        return Err(());
    }
    let root_text = std::str::from_utf8(&root_bytes).map_err(|_| ())?;
    let root = PathBuf::from(root_text);
    let canonical = fs::canonicalize(&root).map_err(|_| ())?;
    if canonical != root || !canonical.is_dir() {
        return Err(());
    }
    let mut relative = PathBuf::new();
    for encoded in &segments[1..] {
        let decoded = percent_decode(encoded)?;
        let component = Path::new(&decoded);
        if decoded.contains(['/', '\0', '\\'])
            || component.components().count() != 1
            || !matches!(component.components().next(), Some(Component::Normal(_)))
        {
            return Err(());
        }
        relative.push(decoded);
    }
    Ok(Capability {
        token,
        root: canonical,
        relative,
    })
}
fn percent_decode(value: &str) -> std::result::Result<String, ()> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex(bytes[index + 1]).ok_or(())?;
            let low = hex(bytes[index + 2]).ok_or(())?;
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| ())
}
fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
fn valid_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
}
#[cfg(unix)]
fn readable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o444 != 0
}
#[cfg(not(unix))]
fn readable(_: &fs::Metadata) -> bool {
    true
}
fn resolved(status: OverlayStatus, path: Option<&str>) -> ResolvedOverlay {
    ResolvedOverlay {
        status,
        path: path.unwrap_or_default().to_owned(),
    }
}

pub fn profile_filename(profile_json: &str) -> String {
    let name = serde_json::from_str::<Value>(profile_json)
        .ok()
        .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let mut filename = String::new();
    let mut utf16 = 0;
    for character in name.trim().chars() {
        let width = character.len_utf16();
        if utf16 + width > 96 {
            break;
        }
        utf16 += width;
        if character < '\u{20}' || character == '\u{7f}' || "/\\:*?\"<>|".contains(character) {
            filename.push('_');
        } else {
            filename.push(character);
        }
    }
    let filename = filename.trim().trim_end_matches('.').trim_end().to_owned();
    format!(
        "{}.json",
        if filename.is_empty() {
            "profile"
        } else {
            &filename
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn authorized_assets_resolve_and_missing_is_classified() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("index file.html");
        fs::write(&file, "ok").unwrap();
        let url = overlay_url(file.to_str().unwrap(), "0123456789abcdef0123456789abcdef").unwrap();
        let mut auth = OverlayAuthorization::default();
        assert!(auth.authorize(&url));
        assert_eq!(auth.resolve(&url).status, OverlayStatus::Ready);
        let missing = url.replace("index%20file.html", "missing.html");
        assert_eq!(auth.resolve(&missing).status, OverlayStatus::NotFound);
    }
    #[test]
    fn token_rebinding_and_atomic_batch_are_rejected() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let fa = a.path().join("a");
        let fb = b.path().join("b");
        fs::write(&fa, "a").unwrap();
        fs::write(&fb, "b").unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let ua = overlay_url(fa.to_str().unwrap(), token).unwrap();
        let ub = overlay_url(fb.to_str().unwrap(), token).unwrap();
        let mut auth = OverlayAuthorization::default();
        assert!(!auth.authorize_json(&serde_json::to_string(&[&ua, &ub]).unwrap()));
        assert!(auth.authorize(&ua));
        assert!(!auth.authorize(&ub));
    }
    #[cfg(unix)]
    #[test]
    fn traversal_and_symlink_escape_are_denied() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let entry = root.path().join("index");
        let secret = outside.path().join("secret");
        fs::write(&entry, "ok").unwrap();
        fs::write(&secret, "no").unwrap();
        symlink(&secret, root.path().join("escape")).unwrap();
        let url = overlay_url(entry.to_str().unwrap(), "0123456789abcdef0123456789abcdef").unwrap();
        let mut auth = OverlayAuthorization::default();
        assert!(auth.authorize(&url));
        assert_eq!(
            auth.resolve(&url.replace("index", "escape")).status,
            OverlayStatus::Denied
        );
        assert_eq!(
            auth.resolve(&url.replace("index", "%2e%2e%2fsecret"))
                .status,
            OverlayStatus::Invalid
        );
    }
    #[test]
    fn malformed_capability_urls_are_invalid() {
        let auth = OverlayAuthorization::default();
        for url in [
            "http://example.test/x",
            "vosulay-overlay://UPPER/x/y",
            "vosulay-overlay://0123456789abcdef0123456789abcdef/not+base64/x",
        ] {
            assert_eq!(auth.resolve(url).status, OverlayStatus::Invalid);
        }
    }
}
