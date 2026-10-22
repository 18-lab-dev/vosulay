//! vosu's optional supervised lifecycle; standalone callers never enter this module.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use url::Url;

static EXIT: AtomicBool = AtomicBool::new(false);

pub struct Environment {
    pub token: String,
    pub backend: Url,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Environment {
    pub fn from_env() -> Result<Option<Self>> {
        Self::parse(|name| std::env::var_os(name))
    }
    fn parse(get: impl Fn(&str) -> Option<OsString>) -> Result<Option<Self>> {
        if get("VOSU_TOOL_PROTOCOL").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return Ok(None);
        }
        let required = |name| {
            get(name)
                .filter(|v| !v.is_empty())
                .with_context(|| format!("tool mode requires {name}"))
        };
        let token = required("VOSU_TOOL_TOKEN")?
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid tool token"))?;
        ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid tool token"
        );
        let backend = crate::config::parse_vosu_url(&required("VOSU_URL")?.to_string_lossy())?;
        Ok(Some(Self {
            token,
            backend,
            data: required("VOSU_TOOL_DATA_DIR")?.into(),
            cache: required("VOSU_TOOL_CACHE_DIR")?.into(),
        }))
    }
    pub fn prepare(&self, standalone: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.data)?;
        std::fs::create_dir_all(&self.cache)?;
        let config = self.data.join("config.json");
        // Only import into a completely empty tool data directory. create_new also
        // prevents a concurrent first launch from replacing an existing import.
        if std::fs::read_dir(&self.data)?.next().is_none() && standalone.is_file() {
            let bytes = std::fs::read(standalone)?;
            let _: crate::config::AppConfig =
                serde_json::from_slice(&bytes).context("standalone config import")?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&config)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        Ok(config)
    }
}

pub fn watch_stdin() -> Result<()> {
    let host = std::thread::current();
    std::thread::Builder::new()
        .name("vosulay-tool-stdin".into())
        .spawn(move || {
            let mut buffer = [0; 64];
            while matches!(std::io::stdin().read(&mut buffer), Ok(n) if n > 0) {}
            EXIT.store(true, Ordering::Release);
            host.unpark();
        })?;
    Ok(())
}
pub fn exit_requested() -> bool {
    EXIT.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch(name: &str) -> Option<OsString> {
        Some(
            match name {
                "VOSU_TOOL_PROTOCOL" => "1",
                "VOSU_TOOL_TOKEN" => {
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }
                "VOSU_URL" => "http://127.0.0.1:25050",
                "VOSU_TOOL_DATA_DIR" => "/tmp/tool-data",
                "VOSU_TOOL_CACHE_DIR" => "/tmp/tool-cache",
                _ => return None,
            }
            .into(),
        )
    }
    #[test]
    fn environment_is_opt_in_and_requires_complete_launch() {
        assert!(Environment::parse(|_| None).unwrap().is_none());
        assert!(
            Environment::parse(|name| if name == "VOSU_URL" {
                None
            } else {
                launch(name)
            })
            .is_err()
        );
        assert_eq!(
            Environment::parse(launch).unwrap().unwrap().backend.port(),
            Some(25050)
        );
    }
    #[test]
    fn config_import_is_idempotent_and_non_destructive() {
        let root = std::env::temp_dir().join(format!("vosulay-import-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("standalone.json");
        let bytes = serde_json::to_vec(&crate::config::AppConfig::default()).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let mut env = Environment::parse(launch).unwrap().unwrap();
        env.data = root.join("data");
        env.cache = root.join("cache");
        let imported = env.prepare(&source).unwrap();
        std::fs::write(&imported, b"preserved").unwrap();
        env.prepare(&source).unwrap();
        assert_eq!(std::fs::read(imported).unwrap(), b"preserved");
        assert_eq!(std::fs::read(source).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}
