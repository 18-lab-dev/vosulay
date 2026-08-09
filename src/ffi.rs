use std::io::{self, Write as _};

use anyhow::{Context, Result, bail};
use serde_json::json;
use url::Url;
use vosulay::{
    api::fetch_installed_counters,
    config::AppConfig,
    shell_policy::{self, CliAction},
    ui::manager_document,
};

#[cxx::bridge(namespace = "vosulay")]
mod bridge {
    struct QtShellOptions {
        vosu_url: String,
        monitor: String,
        fps_limit: i32,
        attach: bool,
        chromium_flags: String,
        ipc_command: String,
    }

    enum ManagerMessageKind {
        Invalid,
        Ready,
        SaveConfig,
        RefreshCounters,
        EditingApplied,
        SetEditing,
        ChooseOverlayFile,
        ExportProfile,
        ImportProfile,
        AuthorizeProfileImport,
        OpenSettings,
        OpenAvailable,
    }

    struct ManagerMessage {
        kind: ManagerMessageKind,
        document_id: String,
        save_id: String,
        payload_json: String,
        counter_id: String,
        editing: bool,
        error: String,
    }

    enum IpcCommandKind {
        Invalid,
        ToggleEdit,
        ToggleOverlay,
        Quit,
        Geometry,
    }

    struct IpcCommand {
        kind: IpcCommandKind,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    }

    enum OverlayRequestStatus {
        Ready,
        Invalid,
        Denied,
        NotFound,
    }

    struct OverlayRequest {
        status: OverlayRequestStatus,
        path: String,
    }

    extern "Rust" {
        type OverlayAuthorization;

        fn manager_document_for_qt(fps_limit: i32) -> String;
        fn load_config_for_qt() -> Result<String>;
        fn save_config_for_qt(config_json: &str) -> Result<()>;
        fn fetch_counters_for_qt(base_url: &str) -> Result<String>;
        fn settings_url_for_qt(base_url: &str, counter_id: &str) -> Result<String>;
        fn available_url_for_qt(base_url: &str) -> Result<String>;
        fn unrestricted_shared_array_buffer_enabled_for_qt() -> bool;

        fn limiter_script_for_qt(initial_fps: f64, native_refresh_rate: f64) -> String;
        fn manager_bridge_script_for_qt() -> String;
        fn counter_metadata_script_for_qt(base_url: &str) -> String;
        fn frame_rate_script_for_qt(fps: f64) -> String;
        fn manager_receive_script_for_qt(payload: &str) -> Result<String>;
        fn parse_manager_message_for_qt(message: &str) -> ManagerMessage;
        fn parse_ipc_command_for_qt(message: &str, attach: bool) -> IpcCommand;
        fn overlay_url_for_qt(path: &str, token: &str) -> Result<String>;
        fn new_overlay_authorization() -> Box<OverlayAuthorization>;
        fn authorize_overlay_url(authorization: &mut OverlayAuthorization, url: &str) -> bool;
        fn authorize_overlay_urls(
            authorization: &mut OverlayAuthorization,
            urls_json: &str,
        ) -> bool;
        fn resolve_overlay_url(authorization: &OverlayAuthorization, url: &str) -> OverlayRequest;
        fn profile_filename_for_qt(profile_json: &str) -> String;
        fn config_has_active_overlays_for_qt(config_json: &str) -> bool;
    }

    unsafe extern "C++" {
        include!("vosulay/src/qt/qt_shell.h");

        fn run_qt_application(arguments_json: &str, options: &QtShellOptions) -> i32;
    }
}

struct OverlayAuthorization(shell_policy::OverlayAuthorization);

pub fn run_qt_application(arguments_json: &str) -> i32 {
    let mut arguments: Vec<String> = match serde_json::from_str(arguments_json) {
        Ok(arguments) => arguments,
        Err(_) => {
            eprintln!("vosulay: invalid argument list");
            return 2;
        }
    };
    if arguments.is_empty() {
        arguments.push("vosulay".to_owned());
    }
    let unrestricted = unrestricted_shared_array_buffer_enabled_for_qt();
    let chromium_flags = std::env::var("QTWEBENGINE_CHROMIUM_FLAGS").unwrap_or_default();
    match shell_policy::parse_cli(&arguments, &chromium_flags, unrestricted) {
        CliAction::Print { text, stderr, code } => {
            if stderr {
                let _ = io::stderr().write_all(text.as_bytes());
            } else {
                let _ = io::stdout().write_all(text.as_bytes());
            }
            code
        }
        CliAction::Launch(options) | CliAction::Ipc(options) => {
            let options = bridge::QtShellOptions {
                vosu_url: options.vosu_url,
                monitor: options.monitor,
                fps_limit: options.fps_limit,
                attach: options.attach,
                chromium_flags: options.chromium_flags,
                ipc_command: options.ipc_command,
            };
            bridge::run_qt_application(arguments_json, &options)
        }
    }
}

fn manager_document_for_qt(fps_limit: i32) -> String {
    manager_document((fps_limit >= 0).then_some(fps_limit as u32))
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

fn unrestricted_shared_array_buffer_enabled_for_qt() -> bool {
    AppConfig::load()
        .map(|config| config.settings.allow_unrestricted_shared_array_buffer)
        .unwrap_or_default()
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
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn limiter_script_for_qt(initial_fps: f64, native_refresh_rate: f64) -> String {
    shell_policy::limiter_script(initial_fps, native_refresh_rate)
}
fn manager_bridge_script_for_qt() -> String {
    shell_policy::MANAGER_BRIDGE_SCRIPT.to_owned()
}
fn counter_metadata_script_for_qt(base_url: &str) -> String {
    shell_policy::counter_metadata_script(base_url)
}
fn frame_rate_script_for_qt(fps: f64) -> String {
    shell_policy::frame_rate_script(fps)
}
fn manager_receive_script_for_qt(payload: &str) -> Result<String> {
    shell_policy::manager_receive_script(payload)
}

fn parse_manager_message_for_qt(message: &str) -> bridge::ManagerMessage {
    let parsed = shell_policy::parse_manager_message(message);
    bridge::ManagerMessage {
        kind: match parsed.kind {
            shell_policy::ManagerKind::Invalid => bridge::ManagerMessageKind::Invalid,
            shell_policy::ManagerKind::Ready => bridge::ManagerMessageKind::Ready,
            shell_policy::ManagerKind::SaveConfig => bridge::ManagerMessageKind::SaveConfig,
            shell_policy::ManagerKind::RefreshCounters => {
                bridge::ManagerMessageKind::RefreshCounters
            }
            shell_policy::ManagerKind::EditingApplied => bridge::ManagerMessageKind::EditingApplied,
            shell_policy::ManagerKind::SetEditing => bridge::ManagerMessageKind::SetEditing,
            shell_policy::ManagerKind::ChooseOverlayFile => {
                bridge::ManagerMessageKind::ChooseOverlayFile
            }
            shell_policy::ManagerKind::ExportProfile => bridge::ManagerMessageKind::ExportProfile,
            shell_policy::ManagerKind::ImportProfile => bridge::ManagerMessageKind::ImportProfile,
            shell_policy::ManagerKind::AuthorizeProfileImport => {
                bridge::ManagerMessageKind::AuthorizeProfileImport
            }
            shell_policy::ManagerKind::OpenSettings => bridge::ManagerMessageKind::OpenSettings,
            shell_policy::ManagerKind::OpenAvailable => bridge::ManagerMessageKind::OpenAvailable,
        },
        document_id: parsed.document_id,
        save_id: parsed.save_id,
        payload_json: parsed.payload_json,
        counter_id: parsed.counter_id,
        editing: parsed.editing,
        error: parsed.error,
    }
}

fn parse_ipc_command_for_qt(message: &str, attach: bool) -> bridge::IpcCommand {
    let parsed = shell_policy::parse_ipc_command(message, attach);
    bridge::IpcCommand {
        kind: match parsed.kind {
            shell_policy::IpcKind::Invalid => bridge::IpcCommandKind::Invalid,
            shell_policy::IpcKind::ToggleEdit => bridge::IpcCommandKind::ToggleEdit,
            shell_policy::IpcKind::ToggleOverlay => bridge::IpcCommandKind::ToggleOverlay,
            shell_policy::IpcKind::Quit => bridge::IpcCommandKind::Quit,
            shell_policy::IpcKind::Geometry => bridge::IpcCommandKind::Geometry,
        },
        x: parsed.x,
        y: parsed.y,
        width: parsed.width,
        height: parsed.height,
    }
}

fn overlay_url_for_qt(path: &str, token: &str) -> Result<String> {
    shell_policy::overlay_url(path, token)
}
fn new_overlay_authorization() -> Box<OverlayAuthorization> {
    Box::new(OverlayAuthorization(
        shell_policy::OverlayAuthorization::default(),
    ))
}
fn authorize_overlay_url(authorization: &mut OverlayAuthorization, url: &str) -> bool {
    authorization.0.authorize(url)
}
fn authorize_overlay_urls(authorization: &mut OverlayAuthorization, urls_json: &str) -> bool {
    authorization.0.authorize_json(urls_json)
}
fn resolve_overlay_url(authorization: &OverlayAuthorization, url: &str) -> bridge::OverlayRequest {
    let resolved = authorization.0.resolve(url);
    bridge::OverlayRequest {
        status: match resolved.status {
            shell_policy::OverlayStatus::Ready => bridge::OverlayRequestStatus::Ready,
            shell_policy::OverlayStatus::Invalid => bridge::OverlayRequestStatus::Invalid,
            shell_policy::OverlayStatus::Denied => bridge::OverlayRequestStatus::Denied,
            shell_policy::OverlayStatus::NotFound => bridge::OverlayRequestStatus::NotFound,
        },
        path: resolved.path,
    }
}
fn profile_filename_for_qt(profile_json: &str) -> String {
    shell_policy::profile_filename(profile_json)
}
fn config_has_active_overlays_for_qt(config_json: &str) -> bool {
    shell_policy::config_has_active_overlays(config_json)
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
