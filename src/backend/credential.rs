//! vosu's broker token. vosu writes a fresh token to `broker.json` each time it starts; presenting
//! it on a `/websocket/commands` upgrade gives that socket the write permission of vosu's own
//! origin, so counters inside vosulay can save their settings without vosulay forging `Origin`.
//! The token only ever goes to a loopback vosu whose listener origin matches the file.
use std::{io::Read, path::PathBuf, sync::Mutex};

use serde::Deserialize;
use url::{Host, Url};

const MAX_FILE: u64 = 4096;

pub(crate) struct BrokerCredential {
    path: PathBuf,
    backend: Url,
    // Only a token is cached: without one, the next connection reads the file again, so a vosu
    // started after vosulay is picked up.
    cached: Mutex<Option<String>>,
}

impl BrokerCredential {
    /// The token source for `backend`, or `None` when `backend` is not on this machine.
    pub(crate) fn for_backend(backend: &Url) -> Option<Self> {
        Self::at(token_path()?, backend)
    }

    pub(crate) fn at(path: PathBuf, backend: &Url) -> Option<Self> {
        loopback(backend).then(|| Self {
            path,
            backend: backend.clone(),
            cached: Mutex::new(None),
        })
    }

    pub(crate) fn token(&self) -> Option<String> {
        let mut cached = self.lock();
        if cached.is_none() {
            *cached = self.read();
        }
        cached.clone()
    }

    /// After vosu rejected the cached token: it restarted with a new one, or stopped.
    pub(crate) fn reread(&self) -> Option<String> {
        let fresh = self.read();
        self.lock().clone_from(&fresh);
        fresh
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> Option<String> {
        let mut bytes = Vec::new();
        std::fs::File::open(&self.path)
            .ok()?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_FILE {
            return None;
        }
        parse(&bytes, &self.backend)
    }
}

/// Linux: `$XDG_RUNTIME_DIR/vosu/broker.json`, else `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/`.
/// Windows: `%LOCALAPPDATA%\vosu\broker.json`.
fn token_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let directory = directories::BaseDirs::new()?.data_local_dir().to_owned();
    #[cfg(not(windows))]
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.config_dir().to_owned()))?;
    Some(directory.join("vosu").join("broker.json"))
}

fn parse(bytes: &[u8], backend: &Url) -> Option<String> {
    #[derive(Deserialize)]
    struct File {
        version: u32,
        origin: String,
        token: String,
    }
    let file: File = serde_json::from_slice(bytes).ok()?;
    let origin = Url::parse(&file.origin).ok()?;
    (file.version == 1
        && file.token.len() == 64
        && file
            .token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && same_listener(&origin, backend))
    .then_some(file.token)
}

fn loopback(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && match url.host() {
            Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        }
}

/// `localhost` and `127.0.0.1` name the same listener; anything else must match exactly.
fn same_listener(origin: &Url, backend: &Url) -> bool {
    let host = |url: &Url| match url.host() {
        Some(Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost") => {
            Some("127.0.0.1".to_owned())
        }
        host => host.map(|host| host.to_string()),
    };
    loopback(origin)
        && loopback(backend)
        && origin.scheme() == backend.scheme()
        && origin.port_or_known_default() == backend.port_or_known_default()
        && host(origin) == host(backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn file(origin: &str, token: &str) -> Vec<u8> {
        format!(r#"{{"version":1,"origin":"{origin}","token":"{token}"}}"#).into_bytes()
    }

    #[test]
    fn token_matches_only_the_configured_loopback_listener() {
        let backend = Url::parse("http://127.0.0.1:24050/").unwrap();
        let localhost = Url::parse("http://localhost:24050/").unwrap();
        let ok = file("http://127.0.0.1:24050", TOKEN);
        assert_eq!(parse(&ok, &backend).as_deref(), Some(TOKEN));
        assert_eq!(parse(&ok, &localhost).as_deref(), Some(TOKEN));
        assert_eq!(
            parse(&file("http://localhost:24050", TOKEN), &backend).as_deref(),
            Some(TOKEN)
        );
        for rejected in [
            file("http://127.0.0.1:24051", TOKEN),
            file("https://127.0.0.1:24050", TOKEN),
            file("http://127.0.0.2:24050", TOKEN),
            file("http://192.168.1.2:24050", TOKEN),
            file("http://127.0.0.1:24050", &TOKEN.to_uppercase()),
            file("http://127.0.0.1:24050", &TOKEN[1..]),
            br#"{"version":2,"origin":"http://127.0.0.1:24050","token":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#.to_vec(),
            b"not json".to_vec(),
        ] {
            assert_eq!(parse(&rejected, &backend), None, "{}", String::from_utf8_lossy(&rejected));
        }
    }

    #[test]
    fn non_loopback_backends_never_get_a_token_source() {
        let path = PathBuf::from("/nonexistent/broker.json");
        for backend in [
            "http://192.168.1.2:24050/",
            "http://vosu.example:24050/",
            "http://[2001:db8::1]:24050/",
        ] {
            assert!(BrokerCredential::at(path.clone(), &Url::parse(backend).unwrap()).is_none());
        }
        for backend in [
            "http://127.0.0.1:24050/",
            "http://127.1.2.3:24050/",
            "http://LOCALHOST:24050/",
            "http://[::1]:24050/",
        ] {
            assert!(BrokerCredential::at(path.clone(), &Url::parse(backend).unwrap()).is_some());
        }
    }

    #[test]
    fn missing_file_is_read_again_and_rejected_tokens_are_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.json");
        let backend = Url::parse("http://127.0.0.1:24050/").unwrap();
        let credential = BrokerCredential::at(path.clone(), &backend).unwrap();
        assert_eq!(credential.token(), None);
        std::fs::write(&path, file("http://127.0.0.1:24050", TOKEN)).unwrap();
        assert_eq!(credential.token().as_deref(), Some(TOKEN));
        let next = TOKEN.replace('0', "f");
        std::fs::write(&path, file("http://127.0.0.1:24050", &next)).unwrap();
        // The cached token stays until vosu rejects it.
        assert_eq!(credential.token().as_deref(), Some(TOKEN));
        assert_eq!(credential.reread().as_deref(), Some(next.as_str()));
        assert_eq!(credential.token().as_deref(), Some(next.as_str()));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(credential.reread(), None);
    }
}
