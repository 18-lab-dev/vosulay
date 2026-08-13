use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use url::Url;

const DEFAULT_VOSU_URL: &str = "http://127.0.0.1:24050";
const UNLIMITED_FPS_FLAG: &str = "--disable-frame-rate-limit";
const ENABLE_FEATURES: &str = "--enable-features=";
const DISABLE_FEATURES: &str = "--disable-features=";
const ENABLE_BLINK_FEATURES: &str = "--enable-blink-features=";
const DISABLE_BLINK_FEATURES: &str = "--disable-blink-features=";
const SHARED_ARRAY_BUFFER_FEATURE: &str = "SharedArrayBufferUnrestrictedAccessAllowed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOptions {
    pub vosu_url: String,
    pub monitor: String,
    pub fps_limit: i32,
    pub attach: bool,
    pub chromium_flags: String,
    pub ipc_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Launch(ShellOptions),
    Ipc(ShellOptions),
    Print {
        text: String,
        stderr: bool,
        code: i32,
    },
}

pub fn parse_cli(arguments: &[String], environment_flags: &str, unrestricted: bool) -> CliAction {
    let mut values = arguments.iter().skip(1).peekable();
    let mut vosu_url = DEFAULT_VOSU_URL.to_owned();
    let mut monitor = String::new();
    let mut fps_value: Option<String> = None;
    let mut attach = false;
    let mut help = false;
    let mut version = false;
    let mut edit = false;
    let mut overlay = false;
    let mut quit = false;
    let mut geometry: Option<String> = None;
    let mut options_done = false;

    while let Some(argument) = values.next() {
        if options_done || !argument.starts_with('-') || argument == "-" {
            continue;
        }
        if argument == "--" {
            options_done = true;
            continue;
        }
        let (name, attached) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(a, b)| (a, Some(b)));
        let mut take_value = |display: &str| -> std::result::Result<String, String> {
            if let Some(value) = attached {
                return Ok(value.to_owned());
            }
            values
                .next()
                .cloned()
                .ok_or_else(|| format!("Missing value after '{display}'."))
        };
        let result = match name {
            "-h" | "--help" | "--help-all" => {
                help = true;
                Ok(())
            }
            "-v" | "--version" => {
                version = true;
                Ok(())
            }
            "-a" | "--attach" => {
                attach = true;
                Ok(())
            }
            "-e" | "--edit" => {
                edit = true;
                Ok(())
            }
            "-i" | "--ipc-toggle-overlay" => {
                overlay = true;
                Ok(())
            }
            "-q" | "--ipc-quit" => {
                quit = true;
                Ok(())
            }
            "-t" | "--vosu-url" => take_value(name).map(|v| vosu_url = v),
            "-m" | "--monitor" => take_value(name).map(|v| monitor = v),
            "-f" | "--fps-limit" => take_value(name).map(|v| fps_value = Some(v)),
            "--ipc-geometry" => take_value(name).map(|v| geometry = Some(v)),
            _ if name.starts_with("-t") && name.len() > 2 => {
                vosu_url = name[2..].trim_start_matches('=').to_owned();
                Ok(())
            }
            _ if name.starts_with("-m") && name.len() > 2 => {
                monitor = name[2..].trim_start_matches('=').to_owned();
                Ok(())
            }
            _ if name.starts_with("-f") && name.len() > 2 => {
                fps_value = Some(name[2..].trim_start_matches('=').to_owned());
                Ok(())
            }
            _ => Err(format!(
                "Unknown option '{}'.",
                name.trim_start_matches('-')
            )),
        };
        if let Err(error) = result {
            return CliAction::Print {
                text: format!("vosulay: {error}\n"),
                stderr: true,
                code: 2,
            };
        }
    }

    if help {
        return CliAction::Print {
            text: help_text(arguments.first().map_or("vosulay", String::as_str)),
            stderr: false,
            code: 0,
        };
    }
    if version {
        return CliAction::Print {
            text: format!("vosulay {}\n", env!("CARGO_PKG_VERSION")),
            stderr: false,
            code: 0,
        };
    }

    let fps_limit = match fps_value.as_deref().map(str::trim) {
        None => -1,
        Some(value) if value.eq_ignore_ascii_case("auto") || value == "0" => 0,
        Some(value) => match value.parse::<i32>() {
            Ok(value) if value > 0 => value,
            _ => {
                return CliAction::Print {
                    text: "vosulay: invalid fps limit; expected auto, 0, or a positive integer\n"
                        .to_owned(),
                    stderr: true,
                    code: 2,
                };
            }
        },
    };
    let chromium_fps = raw_fps_limit(arguments).unwrap_or(-1);
    let chromium_flags = configure_chromium_flags(environment_flags, chromium_fps, unrestricted);
    let mut options = ShellOptions {
        vosu_url,
        monitor,
        fps_limit,
        attach,
        chromium_flags,
        ipc_command: String::new(),
    };
    let command = if edit {
        Some(json!({"command": "toggle-edit"}))
    } else if overlay {
        Some(json!({"command": "toggle-overlay"}))
    } else if quit {
        Some(json!({"command": "quit"}))
    } else if let Some(value) = geometry {
        match parse_geometry(&value) {
            Some([x, y, width, height]) => Some(
                json!({"command": "geometry", "x": x, "y": y, "width": width, "height": height}),
            ),
            None => {
                return CliAction::Print {
                    text: "vosulay: --ipc-geometry expects x,y,w,h with positive size\n".to_owned(),
                    stderr: true,
                    code: 2,
                };
            }
        }
    } else {
        None
    };
    if let Some(command) = command {
        options.ipc_command =
            serde_json::to_string(&command).expect("serializing a static IPC command cannot fail");
        CliAction::Ipc(options)
    } else {
        CliAction::Launch(options)
    }
}

pub fn should_start_remote_server(arguments: &[String]) -> bool {
    matches!(parse_cli(arguments, "", false), CliAction::Launch(_))
}

fn help_text(program: &str) -> String {
    format!(
        concat!(
            "Usage: {program} [options]\n",
            "Native vosu overlay manager\n",
            "\n",
            "Options:\n",
            "  -h, --help                Displays help on commandline options.\n",
            "  --help-all                Displays help, including generic Qt options.\n",
            "  -v, --version             Displays version information.\n",
            "  -t, --vosu-url <url>      Base URL of the vosu instance\n",
            "  -m, --monitor <monitor>   Monitor index or name for vosulay\n",
            "  -f, --fps-limit <fps>     Animation frame limit; use auto or 0 for screen\n",
            "                            refresh\n",
            "  -a, --attach              Attach vosulay to geometry received over IPC\n",
            "  -e, --edit                Toggle editing in the running vosulay\n",
            "  -i, --ipc-toggle-overlay  Toggle visibility of the running vosulay\n",
            "  -q, --ipc-quit            Quit the running vosulay\n",
            "  --ipc-geometry <x,y,w,h>  Set attached vosulay geometry as x,y,w,h\n",
        ),
        program = program
    )
}

fn parse_geometry(value: &str) -> Option<[i32; 4]> {
    let mut parts = value.split(',').map(str::trim);
    let values = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    (parts.next().is_none() && values[2] > 0 && values[3] > 0).then_some(values)
}

fn raw_fps_limit(arguments: &[String]) -> Option<i32> {
    let mut index = 1;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            break;
        }
        let value = if matches!(argument.as_str(), "--fps-limit" | "-f") {
            index += 1;
            arguments.get(index)?.as_str()
        } else if let Some(value) = argument.strip_prefix("--fps-limit=") {
            value
        } else if let Some(value) = argument
            .strip_prefix("-f")
            .filter(|value| !value.is_empty())
        {
            value.strip_prefix('=').unwrap_or(value)
        } else {
            index += 1;
            continue;
        };
        let value = value.trim();
        if value.eq_ignore_ascii_case("auto") {
            return Some(0);
        }
        return value.parse().ok();
    }
    Some(-1)
}

pub fn configure_chromium_flags(flags: &str, fps_limit: i32, unrestricted: bool) -> String {
    let mut arguments = parse_chromium_flags(flags);
    remove_disabled_webgpu_features(&mut arguments);
    let enabled_index = arguments
        .iter()
        .rposition(|value| value.starts_with(ENABLE_FEATURES));
    if let Some(index) = enabled_index {
        for feature in ["Vulkan", "WebGPUService"] {
            let present = arguments[index][ENABLE_FEATURES.len()..]
                .split(',')
                .any(|value| chromium_feature_name(value) == feature);
            if !present {
                if arguments[index].len() > ENABLE_FEATURES.len() {
                    arguments[index].push(',');
                }
                arguments[index].push_str(feature);
            }
        }
    } else {
        arguments.push(format!("{ENABLE_FEATURES}Vulkan,WebGPUService"));
    }
    set_feature_enabled(
        &mut arguments,
        ENABLE_BLINK_FEATURES,
        DISABLE_BLINK_FEATURES,
        SHARED_ARRAY_BUFFER_FEATURE,
        unrestricted,
    );
    if (fps_limit == 0 || fps_limit > 60)
        && !arguments
            .iter()
            .any(|argument| argument == UNLIMITED_FPS_FLAG)
    {
        arguments.push(UNLIMITED_FPS_FLAG.to_owned());
    }
    join_chromium_flags(&arguments)
}

fn parse_chromium_flags(flags: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut argument = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in flags.chars() {
        if character == '"' {
            quoted = !quoted;
            started = true;
        } else if character == ' ' && !quoted {
            if started {
                arguments.push(std::mem::take(&mut argument));
                started = false;
            }
        } else {
            argument.push(character);
            started = true;
        }
    }
    if started && !argument.is_empty() {
        arguments.push(argument);
    }
    arguments
}

fn join_chromium_flags(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| {
            if argument.contains(' ') {
                format!("\"{argument}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn chromium_feature_name(feature: &str) -> &str {
    let trimmed = feature.trim();
    let end = ['<', '.', ':']
        .iter()
        .filter_map(|separator| trimmed.find(*separator))
        .min()
        .unwrap_or(trimmed.len());
    &trimmed[..end]
}

fn remove_disabled_webgpu_features(arguments: &mut [String]) {
    if let Some(index) = arguments
        .iter()
        .rposition(|value| value.starts_with(DISABLE_FEATURES))
    {
        let retained = arguments[index][DISABLE_FEATURES.len()..]
            .split(',')
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && !matches!(chromium_feature_name(value), "Vulkan" | "WebGPUService")
            })
            .collect::<Vec<_>>()
            .join(",");
        arguments[index] = format!("{DISABLE_FEATURES}{retained}");
    }
}

fn set_feature_enabled(
    arguments: &mut Vec<String>,
    enabled_prefix: &str,
    disabled_prefix: &str,
    feature: &str,
    enabled: bool,
) {
    let enabled_index = remove_feature(arguments, enabled_prefix, feature);
    let disabled_index = remove_feature(arguments, disabled_prefix, feature);
    let (prefix, index) = if enabled {
        (enabled_prefix, enabled_index)
    } else {
        (disabled_prefix, disabled_index)
    };
    if let Some(index) = index {
        if !arguments[index].ends_with('=') {
            arguments[index].push(',');
        }
        arguments[index].push_str(feature);
    } else {
        arguments.push(format!("{prefix}{feature}"));
    }
}

fn remove_feature(arguments: &mut [String], prefix: &str, wanted: &str) -> Option<usize> {
    let mut last = None;
    for (index, argument) in arguments.iter_mut().enumerate() {
        if !argument.starts_with(prefix) {
            continue;
        }
        last = Some(index);
        let retained = argument[prefix.len()..]
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty() && chromium_feature_name(value) != wanted)
            .collect::<Vec<_>>()
            .join(",");
        *argument = format!("{prefix}{retained}");
    }
    last
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerKind {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedManagerMessage {
    pub kind: ManagerKind,
    pub document_id: String,
    pub save_id: String,
    pub payload_json: String,
    pub counter_id: String,
    pub editing: bool,
    pub error: String,
}

pub fn parse_manager_message(message: &str) -> ParsedManagerMessage {
    let invalid = |error: &str| ParsedManagerMessage {
        kind: ManagerKind::Invalid,
        document_id: String::new(),
        save_id: String::new(),
        payload_json: String::new(),
        counter_id: String::new(),
        editing: false,
        error: error.to_owned(),
    };
    let Ok(Value::Object(object)) = serde_json::from_str(message) else {
        return invalid("Invalid message from vosulay manager");
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return invalid("Unknown vosulay manager message");
    };
    let mut result = invalid("");
    result.kind = match kind {
        "ready" => {
            result.document_id = object
                .get("__vosulay_document_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            ManagerKind::Ready
        }
        "save-config" => {
            let Some(save_id) = object
                .get("save_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return invalid("Invalid vosulay save ID");
            };
            let Some(config) = object.get("config").filter(|value| value.is_object()) else {
                return invalid("Invalid vosulay configuration");
            };
            result.save_id = save_id.to_owned();
            result.payload_json = serde_json::to_string(config).unwrap();
            ManagerKind::SaveConfig
        }
        "refresh-counters" => ManagerKind::RefreshCounters,
        "editing-applied" => {
            result.editing = object
                .get("editing")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            ManagerKind::EditingApplied
        }
        "set-editing" => {
            result.editing = object
                .get("editing")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            ManagerKind::SetEditing
        }
        "choose-overlay-file" => ManagerKind::ChooseOverlayFile,
        "export-profile" => {
            let Some(profile) = object.get("profile").filter(|value| value.is_object()) else {
                return invalid("The active profile is not a valid JSON object");
            };
            result.payload_json = serde_json::to_string(profile).unwrap();
            ManagerKind::ExportProfile
        }
        "import-profile" => ManagerKind::ImportProfile,
        "authorize-profile-import" => {
            let Some(profile) = object.get("profile").and_then(Value::as_object) else {
                return invalid("Imported profile is not a valid JSON object");
            };
            let Some(overlays) = profile.get("overlays").and_then(Value::as_array) else {
                return invalid("Imported profile does not contain a valid overlays array");
            };
            if overlays.iter().any(|overlay| !overlay.is_object()) {
                return invalid("Imported profile contains an invalid overlay");
            }
            result.payload_json = serde_json::to_string(profile).unwrap();
            ManagerKind::AuthorizeProfileImport
        }
        "open-settings" => {
            result.counter_id = object
                .get("counter_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            ManagerKind::OpenSettings
        }
        "open-available" => ManagerKind::OpenAvailable,
        _ => return invalid("Unknown vosulay manager message"),
    };
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcKind {
    Invalid,
    ToggleEdit,
    ToggleOverlay,
    Quit,
    Geometry,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedIpcCommand {
    pub kind: IpcKind,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn parse_ipc_command(message: &str, attach: bool) -> ParsedIpcCommand {
    let invalid = ParsedIpcCommand {
        kind: IpcKind::Invalid,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    let Ok(Value::Object(object)) = serde_json::from_str(message) else {
        return invalid;
    };
    let integer = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0)
    };
    match object.get("command").and_then(Value::as_str) {
        Some("toggle-edit") => ParsedIpcCommand {
            kind: IpcKind::ToggleEdit,
            ..invalid
        },
        Some("toggle-overlay") => ParsedIpcCommand {
            kind: IpcKind::ToggleOverlay,
            ..invalid
        },
        Some("quit") => ParsedIpcCommand {
            kind: IpcKind::Quit,
            ..invalid
        },
        Some("geometry") if attach => {
            let width = integer("width");
            let height = integer("height");
            if width <= 0 || height <= 0 {
                invalid
            } else {
                ParsedIpcCommand {
                    kind: IpcKind::Geometry,
                    x: integer("x"),
                    y: integer("y"),
                    width,
                    height,
                }
            }
        }
        _ => invalid,
    }
}

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

const LIMITER_SCRIPT: &str = r#"
(() => {
  const initialMaxFps = __VOSULAY_MAX_FPS__;
  const nativeFrameIntervalMs = __VOSULAY_NATIVE_FRAME_INTERVAL_MS__;
  const unboundedScheduler = __VOSULAY_UNBOUNDED_SCHEDULER__;
  if (Number.isFinite(initialMaxFps) && initialMaxFps > 0) {
    let requestedFrameRate = initialMaxFps;
    let applyFrameRate = () => {};
    let limiterInstalled = false;

    window.__vosulaySetAnimationFrameRate = (fps) => {
      requestedFrameRate = Number.isFinite(fps) && fps > 0 ? fps : 0;
      applyFrameRate(requestedFrameRate);
    };

    const installAnimationFrameLimiter = () => {
      if (limiterInstalled || typeof window.requestAnimationFrame !== "function") {
        return limiterInstalled;
      }

      let minFrameIntervalMs = requestedFrameRate > 0 ? 1000 / requestedFrameRate : 0;
      const nativeRequestAnimationFrame = window.requestAnimationFrame.bind(window);
      const nativeCancelAnimationFrame =
        typeof window.cancelAnimationFrame === "function"
          ? window.cancelAnimationFrame.bind(window)
          : null;
      const pendingAnimationFrames = new Map();
      let nextAnimationFrameId = 0;
      let scheduledAnimationFrame = null;
      let scheduledTimeout = null;
      let lastTimestamp = null;
      const frameIntervalToleranceMs = 0.5;

      const nativeFrameLeadMs = () =>
        unboundedScheduler
          ? 1
          : Math.max(1, Math.min(nativeFrameIntervalMs, minFrameIntervalMs));

      const clearScheduledTimeout = () => {
        if (scheduledTimeout === null) return;
        window.clearTimeout(scheduledTimeout);
        scheduledTimeout = null;
      };

      const clearScheduledAnimationFrame = () => {
        if (scheduledAnimationFrame === null || nativeCancelAnimationFrame === null) return;
        nativeCancelAnimationFrame(scheduledAnimationFrame);
        scheduledAnimationFrame = null;
      };

      const flushPendingAnimationFrames = (timestamp) => {
        if (pendingAnimationFrames.size === 0) return;
        if (minFrameIntervalMs > 0 && lastTimestamp !== null &&
            timestamp + frameIntervalToleranceMs - lastTimestamp < minFrameIntervalMs) {
          scheduleAnimationFramePump();
          return;
        }
        lastTimestamp = timestamp;
        const callbacks = Array.from(pendingAnimationFrames.values());
        pendingAnimationFrames.clear();
        try {
          for (const callback of callbacks) {
            try {
              callback(timestamp);
            } catch (error) {
              window.setTimeout(() => { throw error; }, 0);
            }
          }
        } finally {
          if (pendingAnimationFrames.size > 0) scheduleAnimationFramePump();
        }
      };

      const resumeAnimationFramePump = () => {
        scheduledTimeout = null;
        scheduleAnimationFramePump();
      };

      const flushNativeAnimationFrames = (timestamp) => {
        scheduledAnimationFrame = null;
        flushPendingAnimationFrames(timestamp);
      };

      const scheduleAnimationFramePump = () => {
        if (pendingAnimationFrames.size === 0 ||
            scheduledAnimationFrame !== null || scheduledTimeout !== null) return;
        if (minFrameIntervalMs > 0 && lastTimestamp !== null) {
          const remainingMs = lastTimestamp + minFrameIntervalMs - performance.now();
          const delayMs = remainingMs - nativeFrameLeadMs();
          if (delayMs > frameIntervalToleranceMs) {
            scheduledTimeout = window.setTimeout(resumeAnimationFramePump, delayMs);
            return;
          }
        }
        scheduledAnimationFrame = nativeRequestAnimationFrame(flushNativeAnimationFrames);
      };

      applyFrameRate = (fps) => {
        clearScheduledTimeout();
        clearScheduledAnimationFrame();
        minFrameIntervalMs = Number.isFinite(fps) && fps > 0 ? 1000 / fps : 0;
        lastTimestamp = null;
        if (pendingAnimationFrames.size > 0) scheduleAnimationFramePump();
      };

      window.requestAnimationFrame = (callback) => {
        const animationFrameId = ++nextAnimationFrameId;
        pendingAnimationFrames.set(animationFrameId, callback);
        scheduleAnimationFramePump();
        return animationFrameId;
      };

      window.cancelAnimationFrame = (animationFrameId) => {
        pendingAnimationFrames.delete(animationFrameId);
        if (pendingAnimationFrames.size === 0) {
          clearScheduledTimeout();
          clearScheduledAnimationFrame();
        }
      };

      limiterInstalled = true;
      applyFrameRate(requestedFrameRate);
      return true;
    };

    if (!installAnimationFrameLimiter()) {
      const deferredInstall = () => installAnimationFrameLimiter();
      document.addEventListener("DOMContentLoaded", deferredInstall, { once: true });
      window.addEventListener("load", deferredInstall, { once: true });
      window.setTimeout(deferredInstall, 0);
    }
  }
})();
"#;

pub fn limiter_script(initial_fps: f64, native_refresh_rate: f64) -> String {
    let initial = finite_fixed(initial_fps, 0.0);
    let refresh = if native_refresh_rate.is_finite() && native_refresh_rate >= 1.0 {
        native_refresh_rate
    } else {
        60.0
    };
    LIMITER_SCRIPT
        .replace("__VOSULAY_MAX_FPS__", &initial)
        .replace(
            "__VOSULAY_NATIVE_FRAME_INTERVAL_MS__",
            &format!("{:.3}", 1000.0 / refresh),
        )
        .replace(
            "__VOSULAY_UNBOUNDED_SCHEDULER__",
            if std::env::var("QTWEBENGINE_CHROMIUM_FLAGS").is_ok_and(|flags| {
                parse_chromium_flags(&flags)
                    .iter()
                    .any(|value| value == UNLIMITED_FPS_FLAG)
            }) {
                "true"
            } else {
                "false"
            },
        )
}
fn finite_fixed(value: f64, fallback: f64) -> String {
    format!("{:.3}", if value.is_finite() { value } else { fallback })
}

pub const MANAGER_BRIDGE_SCRIPT: &str = r#"
(() => {
  const queuedMessages = [];
  const documentId =
    globalThis.crypto?.randomUUID?.() ??
    `${performance.timeOrigin}:${Math.random()}`;
  const serializePayload = (payload) => {
    try {
      const message = JSON.parse(payload);
      if (message && message.type === "ready") {
        message.__vosulay_document_id = documentId;
        return JSON.stringify(message);
      }
    } catch (_) {
    }
    return payload;
  };
  let sender = (payload) => queuedMessages.push(payload);
  document.documentElement.addEventListener("vosulay-bridge-message", () => {
    const payload =
      document.documentElement.getAttribute("data-vosulay-bridge-message");
    if (payload !== null) sender(payload);
  });
  new QWebChannel(qt.webChannelTransport, (channel) => {
    const bridge = channel.objects.vosulayBridge;
    sender = (payload) => bridge.postMessage(serializePayload(payload));
    for (const payload of queuedMessages.splice(0)) sender(payload);
  });
})();
"#;

const COUNTER_METADATA_SCRIPT: &str = r#"
(() => {
  try {
    const vosuOrigin = new URL(__VOSULAY_BASE_URL__[0]).origin;
    if (window.location.origin !== vosuOrigin ||
        typeof window.COUNTER_PATH === "string") {
      return;
    }

    const pathname = window.location.pathname.endsWith("/")
      ? window.location.pathname
      : window.location.pathname.slice(0, window.location.pathname.lastIndexOf("/") + 1);
    const counterPath = pathname
      .split("/")
      .filter(Boolean)
      .map((segment) => decodeURIComponent(segment))
      .join("/");
    if (counterPath) window.COUNTER_PATH = counterPath;
  } catch (_) {
  }
})();
"#;
pub fn counter_metadata_script(base_url: &str) -> String {
    COUNTER_METADATA_SCRIPT.replace(
        "__VOSULAY_BASE_URL__",
        &serde_json::to_string(&[base_url]).expect("string serialization cannot fail"),
    )
}
pub fn frame_rate_script(fps: f64) -> String {
    format!(
        "if (typeof window.__vosulaySetAnimationFrameRate === 'function') window.__vosulaySetAnimationFrameRate({});",
        finite_fixed(fps, 0.0)
    )
}
pub fn manager_receive_script(payload: &str) -> Result<String> {
    let value: Value = serde_json::from_str(payload).context("invalid manager envelope JSON")?;
    let encoded = serde_json::to_string(&value)?;
    Ok(format!(
        "if (typeof window.__VOSULAY_RECEIVE__ === 'function') window.__VOSULAY_RECEIVE__({encoded}); else if (window.vosulay && typeof window.vosulay.receive === 'function') window.vosulay.receive({encoded});"
    ))
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

pub fn config_has_active_overlays(config_json: &str) -> bool {
    let Ok(Value::Object(config)) = serde_json::from_str(config_json) else {
        return true;
    };
    let Some(active) = config
        .get("active_profile")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return true;
    };
    let Some(profiles) = config.get("profiles").and_then(Value::as_array) else {
        return true;
    };
    let mut found = None;
    for profile in profiles {
        let Some(object) = profile.as_object() else {
            return true;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return true;
        };
        let Some(overlays) = object.get("overlays").and_then(Value::as_array) else {
            return true;
        };
        if id == active {
            if found.is_some() {
                return true;
            }
            found = Some(!overlays.is_empty());
        }
    }
    found.unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn chromium_flags_preserve_quotes_and_merge_features() {
        let flags = configure_chromium_flags(
            "--user-agent=\"hello world\" --enable-features=Existing --disable-features=Vulkan,Other",
            144,
            true,
        );
        assert_eq!(
            flags,
            "\"--user-agent=hello world\" --enable-features=Existing,Vulkan,WebGPUService --disable-features=Other --enable-blink-features=SharedArrayBufferUnrestrictedAccessAllowed --disable-frame-rate-limit"
        );
    }
    #[test]
    fn cli_launch_preserves_defaults_and_options() {
        let CliAction::Launch(options) = parse_cli(
            &argv(&[
                "vosulay",
                "--vosu-url=https://vosu.test",
                "-m",
                "DP-1",
                "--fps-limit",
                "auto",
                "--attach",
            ]),
            "",
            false,
        ) else {
            panic!("launch expected")
        };
        assert_eq!(
            (
                options.vosu_url.as_str(),
                options.monitor.as_str(),
                options.fps_limit,
                options.attach,
                options.ipc_command.as_str()
            ),
            ("https://vosu.test", "DP-1", 0, true, "")
        );
    }
    #[test]
    fn cli_invalid_fps_is_usage_error() {
        let CliAction::Print { code, text, stderr } =
            parse_cli(&argv(&["vosulay", "-f", "bad"]), "", false)
        else {
            panic!("error expected")
        };
        assert_eq!(
            (code, stderr, text.as_str()),
            (
                2,
                true,
                "vosulay: invalid fps limit; expected auto, 0, or a positive integer\n"
            )
        );
    }
    #[test]
    fn cli_immediate_precedence_and_serialization() {
        let CliAction::Ipc(options) =
            parse_cli(&argv(&["vosulay", "--ipc-quit", "--edit"]), "", false)
        else {
            panic!("IPC expected")
        };
        assert_eq!(options.ipc_command, r#"{"command":"toggle-edit"}"#);
    }

    #[test]
    fn cli_visibility_toggle_serializes_ipc_command() {
        let CliAction::Ipc(options) =
            parse_cli(&argv(&["vosulay", "--ipc-toggle-overlay"]), "", false)
        else {
            panic!("IPC expected")
        };
        assert_eq!(options.ipc_command, r#"{"command":"toggle-overlay"}"#);
    }

    #[test]
    fn ipc_visibility_toggle_does_not_start_remote_server() {
        assert!(!should_start_remote_server(&argv(&[
            "vosulay",
            "--ipc-toggle-overlay"
        ])));
    }
    #[test]
    fn cli_help_and_version_are_handled_without_launching() {
        for option in ["--help", "--help-all"] {
            let CliAction::Print {
                text: help,
                code: 0,
                stderr: false,
            } = parse_cli(&argv(&["vosulay", option]), "", false)
            else {
                panic!("help expected")
            };
            assert!(help.contains("--help-all"));
        }
        let CliAction::Print {
            text: version,
            code: 0,
            stderr: false,
        } = parse_cli(&argv(&["vosulay", "--version"]), "", false)
        else {
            panic!("version expected")
        };
        assert_eq!(version, format!("vosulay {}\n", env!("CARGO_PKG_VERSION")));
    }
    #[test]
    fn manager_messages_reject_malformed_and_unknown() {
        assert_eq!(
            parse_manager_message("[]").error,
            "Invalid message from vosulay manager"
        );
        assert_eq!(
            parse_manager_message(r#"{"type":"wat"}"#).error,
            "Unknown vosulay manager message"
        );
    }
    #[test]
    fn manager_save_carries_compact_payload() {
        let parsed =
            parse_manager_message(r#"{"type":"save-config","save_id":"s","config":{"x":1}}"#);
        assert_eq!(
            (
                parsed.kind,
                parsed.save_id.as_str(),
                parsed.payload_json.as_str()
            ),
            (ManagerKind::SaveConfig, "s", r#"{"x":1}"#)
        );
    }
    #[test]
    fn manager_parser_owns_profile_shape_validation() {
        assert_eq!(
            parse_manager_message(r#"{"type":"export-profile","profile":[]}"#).error,
            "The active profile is not a valid JSON object"
        );
        assert_eq!(
            parse_manager_message(
                r#"{"type":"authorize-profile-import","profile":{"overlays":[null]}}"#
            )
            .error,
            "Imported profile contains an invalid overlay"
        );
    }
    #[test]
    fn ipc_parses_visibility_toggle_command() {
        assert_eq!(
            parse_ipc_command(r#"{"command":"toggle-overlay"}"#, true).kind,
            IpcKind::ToggleOverlay
        );
    }
    #[test]
    fn ipc_rejects_unknown_malformed_and_detached_geometry() {
        assert_eq!(parse_ipc_command("bad", true).kind, IpcKind::Invalid);
        assert_eq!(
            parse_ipc_command(r#"{"command":"wat"}"#, true).kind,
            IpcKind::Invalid
        );
        assert_eq!(
            parse_ipc_command(r#"{"command":"geometry","width":1,"height":1}"#, false).kind,
            IpcKind::Invalid
        );
    }
    #[test]
    fn scripts_escape_interpolated_strings_and_nonfinite_numbers() {
        assert!(
            counter_metadata_script("https://x.test/\"];alert(1)//")
                .contains(r#"["https://x.test/\"];alert(1)//"]"#)
        );
        assert!(!limiter_script(f64::NAN, f64::INFINITY).contains("NaN"));
        assert!(manager_receive_script(r#"{"text":"</script>\""}"#).is_ok());
    }
    #[test]
    fn profile_filename_and_active_overlay_semantics_match_shell() {
        assert_eq!(
            profile_filename(r#"{"name":" bad/name... "}"#),
            "bad_name.json"
        );
        assert!(!config_has_active_overlays(
            r#"{"active_profile":"a","profiles":[{"id":"a","overlays":[]}]}"#
        ));
        assert!(config_has_active_overlays(
            r#"{"active_profile":"missing","profiles":[]}"#
        ));
    }
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
