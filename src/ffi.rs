use anyhow::{Context, Result, bail};
use serde_json::json;
use url::Url;
use vosulay::{api::fetch_installed_counters, config::AppConfig, ui::manager_document};

#[cxx::bridge(namespace = "vosulay")]
mod bridge {
    extern "Rust" {
        fn manager_document_for_qt(fps_limit: i32) -> String;
        fn load_config_for_qt() -> Result<String>;
        fn save_config_for_qt(config_json: &str) -> Result<()>;
        fn fetch_counters_for_qt(base_url: &str) -> Result<String>;
        fn settings_url_for_qt(base_url: &str, counter_id: &str) -> Result<String>;
        fn available_url_for_qt(base_url: &str) -> Result<String>;
    }

    unsafe extern "C++" {
        include!("vosulay/src/qt/qt_shell.h");

        fn run_qt_application(arguments_json: &str) -> i32;
    }
}

pub fn run_qt_application(arguments_json: &str) -> i32 {
    bridge::run_qt_application(arguments_json)
}

fn manager_document_for_qt(fps_limit: i32) -> String {
    let fps_limit = (fps_limit >= 0).then_some(fps_limit as u32);
    manager_document(fps_limit)
}

fn load_config_for_qt() -> Result<String> {
    let (config, error) = match AppConfig::load() {
        Ok(config) => (config, None),
        Err(error) => (
            AppConfig::default(),
            Some(format!("Could not load configuration: {error:#}")),
        ),
    };

    serde_json::to_string(&json!({ "config": config, "error": error }))
        .context("failed to serialize configuration response")
}

fn save_config_for_qt(config_json: &str) -> Result<()> {
    let mut config: AppConfig =
        serde_json::from_str(config_json).context("failed to parse configuration")?;
    config.normalize();
    config.save().context("failed to save configuration")
}

fn fetch_counters_for_qt(base_url: &str) -> Result<String> {
    let base_url = validate_base_url(base_url)?;
    let counters = fetch_installed_counters(&base_url)?;
    serde_json::to_string(&counters).context("failed to serialize installed counters")
}

fn settings_url_for_qt(base_url: &str, counter_id: &str) -> Result<String> {
    let base_url = validate_base_url(base_url)?;
    let mut url = base_url
        .join("/settings")
        .context("could not construct the counter settings URL")?;
    url.query_pairs_mut().append_pair("overlay", counter_id);
    Ok(url.into())
}

fn available_url_for_qt(base_url: &str) -> Result<String> {
    validate_base_url(base_url)?
        .join("/available")
        .context("could not construct the available counters URL")
        .map(Into::into)
}

fn validate_base_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).context("vosu base URL must be an absolute URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("vosu base URL must use http or https and include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("vosu base URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("vosu base URL must not contain a query or fragment");
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{available_url_for_qt, save_config_for_qt, settings_url_for_qt, validate_base_url};

    #[test]
    fn settings_url_encodes_counter_identifier() {
        let url = settings_url_for_qt("http://127.0.0.1:24050", "counter name&mode")
            .expect("settings URL should be valid");

        assert_eq!(
            url,
            "http://127.0.0.1:24050/settings?overlay=counter+name%26mode"
        );
    }

    #[test]
    fn available_url_replaces_base_path() {
        let url = available_url_for_qt("http://127.0.0.1:24050/prefix")
            .expect("available URL should be valid");

        assert_eq!(url, "http://127.0.0.1:24050/available");
    }

    #[test]
    fn base_url_rejects_credentials() {
        let error = validate_base_url("http://user:secret@127.0.0.1:24050")
            .expect_err("credentials must be rejected");

        assert!(error.to_string().contains("must not contain credentials"));
    }
    #[test]
    fn save_config_reports_invalid_json_with_context() {
        let error =
            save_config_for_qt("{not json").expect_err("invalid configuration must be rejected");

        assert!(error.to_string().contains("failed to parse configuration"));
    }
}
