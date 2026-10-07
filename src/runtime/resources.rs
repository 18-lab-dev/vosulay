use crate::{
    config::{AppConfig, Profile},
    shell_policy::{OverlayAuthorization, OverlayStatus},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
};
use vosulay_browser::scheme::{MAX_RESOURCE_BYTES, Resource, ResourceRoute, ResourceService};

pub struct ProductResources {
    manager: Arc<[u8]>,
    authorization: Mutex<OverlayAuthorization>,
}

impl ProductResources {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let resources = Self {
            manager: crate::ui::manager_document().into_bytes().into(),
            authorization: Mutex::new(OverlayAuthorization::default()),
        };
        resources.authorize_profiles(&config.profiles)?;
        Ok(resources)
    }

    pub fn authorize_profiles(&self, profiles: &[Profile]) -> Result<()> {
        let urls: Vec<_> = profiles
            .iter()
            .flat_map(|profile| &profile.overlays)
            .filter_map(|overlay| overlay.url.as_ref())
            .filter(|url| url.scheme() == "vosulay-overlay")
            .map(|url| url.as_str())
            .collect();
        let encoded = serde_json::to_string(&urls)?;
        ensure!(
            self.authorization
                .lock()
                .map_err(|_| anyhow::anyhow!("local overlay authorization poisoned"))?
                .authorize_json(&encoded),
            "could not authorize local overlays"
        );
        Ok(())
    }

    pub fn authorize_url(&self, url: &str) -> Result<()> {
        ensure!(
            self.authorization
                .lock()
                .map_err(|_| anyhow::anyhow!("local overlay authorization poisoned"))?
                .authorize(url),
            "could not authorize selected local overlay"
        );
        Ok(())
    }
}

impl ResourceService for ProductResources {
    fn load(&self, route: ResourceRoute<'_>) -> Option<Resource> {
        match route {
            ResourceRoute::Manager {
                path: "/index.html",
            } => Some(Resource {
                bytes: self.manager.clone(),
                mime_type: "text/html".into(),
                status: 200,
            }),
            ResourceRoute::Local { capability, path } => {
                let url = format!("vosulay-overlay://{capability}/{path}");
                let resolved = self.authorization.lock().ok()?.resolve(&url);
                if resolved.status != OverlayStatus::Ready {
                    return None;
                }
                let path = Path::new(&resolved.path);
                let bytes = read_bounded(path, MAX_RESOURCE_BYTES).ok()?;
                Some(Resource {
                    bytes: bytes.into(),
                    mime_type: mime_type(path).into(),
                    status: 200,
                })
            }
            _ => None,
        }
    }
}

pub(super) fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= maximum as u64,
        "file is not regular or exceeds the size limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "file grew beyond the size limit");
    Ok(bytes)
}

fn mime_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    for (extensions, mime) in [
        (&["html", "htm"][..], "text/html"),
        (&["css"][..], "text/css"),
        (&["js", "mjs"][..], "text/javascript"),
        (&["json"][..], "application/json"),
        (&["svg"][..], "image/svg+xml"),
        (&["png"][..], "image/png"),
        (&["jpg", "jpeg"][..], "image/jpeg"),
        (&["gif"][..], "image/gif"),
        (&["webp"][..], "image/webp"),
        (&["ico"][..], "image/x-icon"),
        (&["woff"][..], "font/woff"),
        (&["woff2"][..], "font/woff2"),
        (&["ttf"][..], "font/ttf"),
        (&["otf"][..], "font/otf"),
        (&["wasm"][..], "application/wasm"),
        (&["mp3"][..], "audio/mpeg"),
        (&["wav"][..], "audio/wav"),
        (&["ogg"][..], "audio/ogg"),
        (&["webm"][..], "video/webm"),
        (&["mp4"][..], "video/mp4"),
    ] {
        if extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            return mime;
        }
    }
    "application/octet-stream"
}
