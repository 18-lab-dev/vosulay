use anyhow::{Context, Result, ensure};
use directories::BaseDirs;
use std::{
    borrow::Cow,
    collections::HashSet,
    fs::{self, File},
    io::Write,
    iter::Peekable,
    ops::Range,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::CharIndices,
};
use tempfile::NamedTempFile;

const ENVIRONMENT: [&str; 4] = [
    "LD_PRELOAD",
    "VK_ADD_LAYER_PATH",
    "VK_INSTANCE_LAYERS",
    "VK_LAYER_PATH",
];
const VULKAN_LAYER: &str = "VK_LAYER_VOSULAY_overlay";

struct Integration<'a> {
    library: &'a str,
    lookup: &'a str,
    layer_directory: &'a str,
    obs_layer: Option<&'a str>,
    inherited: [Option<&'a str>; 4],
}

pub(super) fn refresh(compositor: &Path, lookup: &Path) {
    let result = (|| {
        let base = BaseDirs::new().context("could not determine the user application directory")?;
        let directories = std::env::var_os("XDG_DATA_DIRS")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let directories: Vec<_> = std::env::split_paths(&directories)
            .filter(|path| path.is_absolute())
            .collect();
        let inherited = ENVIRONMENT.map(|key| std::env::var(key).ok());
        refresh_in(
            base.data_dir(),
            &directories,
            compositor,
            lookup,
            inherited.each_ref().map(|value| value.as_deref()),
        )
    })();
    match result {
        Ok(0) => {}
        Ok(count) => eprintln!(
            "Vosulay: configured compositor launch environment in {count} osu shortcut(s)"
        ),
        Err(error) => eprintln!("Vosulay: could not configure osu shortcuts: {error:#}"),
    }
}

fn refresh_in(
    data_home: &Path,
    directories: &[PathBuf],
    compositor: &Path,
    lookup: &Path,
    inherited: [Option<&str>; 4],
) -> Result<usize> {
    ensure!(
        compositor.is_absolute() && compositor.is_file(),
        "compositor is not an installed absolute file: {}",
        compositor.display()
    );
    let library = compositor
        .to_str()
        .context("compositor path is not UTF-8")?;
    // ld.so separates names on both colons and whitespace; desktop quoting cannot
    // make a library containing either into a single preload entry.
    ensure!(
        !library.contains(':') && !library.chars().any(char::is_whitespace),
        "compositor path cannot be represented in LD_PRELOAD: {library}"
    );
    ensure!(
        lookup.is_absolute() && lookup.is_file(),
        "lookup helper is not an installed absolute file: {}",
        lookup.display()
    );
    let lookup = lookup.to_str().context("lookup helper path is not UTF-8")?;
    ensure!(
        !lookup.contains(':') && !lookup.chars().any(char::is_whitespace),
        "lookup helper path cannot be represented in LD_PRELOAD: {lookup}"
    );
    let applications = data_home.join("applications");
    let layer_directory = data_home.join("vosulay/vulkan");
    let obs_layer =
        obs_layer(std::iter::once(data_home).chain(directories.iter().map(PathBuf::as_path)))?;
    let integration = Integration {
        library,
        lookup,
        layer_directory: layer_directory
            .to_str()
            .context("Vulkan layer directory is not UTF-8")?,
        obs_layer: obs_layer.as_deref(),
        inherited,
    };
    ensure!(
        !integration.layer_directory.contains(':'),
        "Vulkan layer directory contains a path separator"
    );
    let mut layer_written = false;
    let mut seen = HashSet::new();
    let mut changed = 0;
    for root in std::iter::once(data_home).chain(directories.iter().map(PathBuf::as_path)) {
        let root = root.join("applications");
        let mut entries = Vec::new();
        match desktop_files(&root, &mut entries) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("Vosulay: could not scan {}: {error:#}", root.display());
                continue;
            }
        }
        entries.sort_unstable();
        for path in entries {
            let relative = path.strip_prefix(&root)?;
            let Some(relative) = relative.to_str() else {
                continue;
            };
            let id = relative.replace('/', "-");
            // Masking happens before parsing: a user's Hidden=true entry must not
            // be resurrected by copying the corresponding system launcher.
            if !seen.insert(id.clone()) {
                continue;
            }
            let result = (|| -> Result<bool> {
                let original = fs::read_to_string(&path)?;
                let Some(updated) = rewrite_entry(&original, &integration)? else {
                    return Ok(false);
                };
                if !layer_written {
                    write_layer(&layer_directory, library)?;
                    layer_written = true;
                }
                let destination = if root == applications {
                    path.clone()
                } else {
                    applications.join(id)
                };
                fs::create_dir_all(
                    destination
                        .parent()
                        .context("desktop entry has no parent")?,
                )?;
                let mut temporary = NamedTempFile::new_in(destination.parent().unwrap())?;
                temporary.write_all(updated.as_bytes())?;
                temporary
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(
                        fs::metadata(&path)?.permissions().mode() & 0o777,
                    ))?;
                temporary.as_file().sync_all()?;
                // Do not replace a shortcut edited during this scan with an older
                // snapshot, or overwrite a user override created in the meantime.
                if root == applications {
                    ensure!(
                        fs::read_to_string(&path)? == original,
                        "shortcut changed during refresh"
                    );
                    temporary
                        .persist(&destination)
                        .map_err(|error| error.error)?;
                } else {
                    temporary
                        .persist_noclobber(&destination)
                        .map_err(|error| error.error)?;
                }
                File::open(destination.parent().unwrap())?.sync_all()?;
                Ok(true)
            })();
            match result {
                Ok(true) => changed += 1,
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Vosulay: could not configure {}: {error:#}", path.display())
                }
            }
        }
    }
    Ok(changed)
}

fn obs_layer<'a>(directories: impl Iterator<Item = &'a Path>) -> Result<Option<String>> {
    for root in directories {
        let directory = root.join("vulkan/implicit_layer.d");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", directory.display()));
            }
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            if let Some(name) = manifest["layer"]["name"].as_str()
                && name.starts_with("VK_LAYER_OBS_vkcapture")
                && !name.ends_with("_32")
            {
                return Ok(Some(name.to_owned()));
            }
        }
    }
    Ok(None)
}

fn write_layer(directory: &Path, library: &str) -> Result<()> {
    let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../crates/overlay-linux/VkLayer_vosulay_overlay.json"
    ))?;
    manifest["layer"]["library_path"] = library.into();
    let manifest = serde_json::to_vec_pretty(&manifest)?;
    let destination = directory.join("VkLayer_vosulay_overlay.json");
    if fs::read(&destination).is_ok_and(|current| current == manifest) {
        return Ok(());
    }
    fs::create_dir_all(directory)?;
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(&manifest)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn desktop_files(directory: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            desktop_files(&entry.path(), output)?;
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "desktop")
        {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn rewrite_entry(input: &str, integration: &Integration<'_>) -> Result<Option<String>> {
    let mut main = false;
    let mut application = false;
    let mut hidden = false;
    let mut dbus = false;
    let mut exec = None;
    for line in input.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
        }
        if !main {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Type" => application = value == "Application",
                "Hidden" => hidden = value == "true",
                "DBusActivatable" => dbus = value == "true",
                "Exec" => exec = Some(value),
                _ => {}
            }
        }
    }
    if !application || hidden {
        return Ok(None);
    }
    let Some(exec) = exec else { return Ok(None) };
    if !exec
        .as_bytes()
        .windows(3)
        .any(|word| word.eq_ignore_ascii_case(b"osu"))
    {
        return Ok(None);
    }
    let Some(_) = rewrite_exec(exec, integration)? else {
        return Ok(None);
    };
    ensure!(
        !dbus,
        "D-Bus activation bypasses Exec; cannot configure this osu shortcut's preload"
    );
    let mut output = String::with_capacity(input.len() + integration.library.len());
    let mut group = false;
    for line in input.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            group = trimmed == "[Desktop Entry]" || trimmed.starts_with("[Desktop Action ");
        }
        if group
            && let Some((key, command)) = trimmed.split_once('=')
            && key.trim() == "Exec"
            && let Some(updated) = rewrite_exec(command, integration)?
        {
            let start = line.find('=').unwrap() + 1;
            output.push_str(&line[..start]);
            output.push_str(&updated);
            if line.ends_with("\r\n") {
                output.push_str("\r\n");
            } else if line.ends_with('\n') {
                output.push('\n');
            }
            continue;
        }
        output.push_str(line);
    }
    Ok((output != input).then_some(output))
}

struct Token {
    value: String,
    raw: Range<usize>,
}

fn value_char(chars: &mut Peekable<CharIndices<'_>>) -> Option<(usize, char, usize)> {
    let (start, mut character) = chars.next()?;
    let mut end = start + character.len_utf8();
    if character == '\\'
        && let Some(&(index, next)) = chars.peek()
    {
        let decoded = match next {
            's' => Some(' '),
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '\\' => Some('\\'),
            _ => None,
        };
        if let Some(decoded) = decoded {
            chars.next();
            character = decoded;
            end = index + next.len_utf8();
        }
    }
    Some((start, character, end))
}

fn tokens(command: &str) -> Result<Vec<Token>> {
    let mut chars = command.char_indices().peekable();
    let mut output = Vec::new();
    let mut token: Option<Token> = None;
    let mut quote = None;
    let mut escaped = false;
    while let Some((start, character, end)) = value_char(&mut chars) {
        if !escaped && quote.is_none() && character.is_ascii_whitespace() {
            if let Some(token) = token.take() {
                output.push(token);
            }
            continue;
        }
        let token = token.get_or_insert_with(|| Token {
            value: String::new(),
            raw: start..end,
        });
        token.raw.end = end;
        if escaped {
            if quote == Some('"') && !matches!(character, '"' | '\\' | '`' | '$' | '\n') {
                token.value.push('\\');
            }
            token.value.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '"' | '\'') {
            quote = Some(character);
        } else {
            token.value.push(character);
        }
    }
    ensure!(
        !escaped && quote.is_none(),
        "unterminated desktop Exec quoting"
    );
    if let Some(token) = token {
        output.push(token);
    }
    Ok(output)
}

fn basename(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}
fn native_game(value: &str) -> bool {
    let name = basename(value);
    [
        "osu",
        "osu!",
        "osulazer",
        "osu-lazer",
        "osu.AppImage",
        "osu!lazer.AppImage",
    ]
    .iter()
    .any(|expected| name.eq_ignore_ascii_case(expected))
        || (name.get(..4).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("osu-") || prefix.eq_ignore_ascii_case("osu!")
        }) && name
            .get(name.len().saturating_sub(9)..)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(".AppImage")))
}

fn rewrite_exec(command: &str, integration: &Integration<'_>) -> Result<Option<String>> {
    let tokens = tokens(command)?;
    let mut index = 0;
    let mut remove = HashSet::new();
    let mut environment = integration.inherited.map(|value| (value, false));
    let mut last_env = None;
    loop {
        let Some(token) = tokens.get(index) else {
            return Ok(None);
        };
        match basename(&token.value) {
            "env" => {
                index += 1;
                while let Some(token) = tokens.get(index) {
                    match token.value.as_str() {
                        "--" => {
                            index += 1;
                            break;
                        }
                        "-i" | "--ignore-environment" => environment.fill((None, false)),
                        "-u" | "--unset" | "-C" | "--chdir" | "-a" | "--argv0" => {
                            let argument = tokens
                                .get(index + 1)
                                .context("env option requires an argument")?;
                            if matches!(token.value.as_str(), "-u" | "--unset")
                                && let Some(position) =
                                    ENVIRONMENT.iter().position(|key| *key == argument.value)
                            {
                                environment[position] = (None, false);
                            }
                            index += 2;
                            continue;
                        }
                        value if value.starts_with("--unset=") => {
                            if let Some(position) = ENVIRONMENT
                                .iter()
                                .position(|key| Some(*key) == value.strip_prefix("--unset="))
                            {
                                environment[position] = (None, false);
                            }
                        }
                        value if value.starts_with("--chdir=") || value.starts_with("--argv0=") => {
                        }
                        value if value.starts_with('-') => return Ok(None),
                        _ => break,
                    }
                    index += 1;
                }
                last_env = Some(index);
                while let Some(token) = tokens.get(index)
                    && let Some((key, value)) = token.value.split_once('=')
                {
                    if let Some(position) = ENVIRONMENT.iter().position(|expected| *expected == key)
                    {
                        environment[position] = (Some(value), true);
                        remove.insert(index);
                    }
                    index += 1;
                }
            }
            "gamemoderun" | "prime-run" => index += 1,
            "mangohud" => {
                index += 1;
                if tokens
                    .get(index)
                    .is_some_and(|token| token.value == "--dlsym")
                {
                    index += 1;
                }
            }
            "gamescope" => {
                let Some(delimiter) = tokens[index + 1..]
                    .iter()
                    .position(|token| token.value == "--")
                else {
                    return Ok(None);
                };
                index += delimiter + 2;
            }
            _ => break,
        }
    }
    let Some(program) = tokens.get(index) else {
        return Ok(None);
    };
    let wine = matches!(basename(&program.value), "wine" | "wine64")
        && tokens.get(index + 1).is_some_and(|token| {
            basename(&token.value).eq_ignore_ascii_case("osu!.exe")
                || basename(&token.value).eq_ignore_ascii_case("osulazer.exe")
        });
    let winello = basename(&program.value) == "osu-wine"
        && tokens
            .get(index + 1)
            .is_none_or(|token| !token.value.starts_with('-') || token.value == "--devserver");
    if !native_game(&program.value) && !wine && !winello {
        return Ok(None);
    }
    let decoded = environment.map(|(value, field_codes)| {
        let value = value.unwrap_or_default();
        if field_codes && value.contains("%%") {
            Cow::Owned(value.replace("%%", "%"))
        } else {
            Cow::Borrowed(value)
        }
    });
    let mut libraries: Vec<&str> = decoded[0]
        .split(|character: char| character == ':' || character.is_ascii_whitespace())
        .filter(|entry| {
            !entry.is_empty()
                && *entry != integration.library
                && *entry != integration.lookup
                && !vosulay_linux::injector::managed_library(entry)
        })
        .collect();
    libraries.insert(0, integration.lookup);
    libraries.push(integration.library);
    let path_index = if decoded[3].is_empty() { 1 } else { 3 };
    let mut directories: Vec<&str> = decoded[path_index]
        .split(':')
        .filter(|entry| !entry.is_empty() && *entry != integration.layer_directory)
        .collect();
    directories.push(integration.layer_directory);
    let capture = libraries
        .iter()
        .any(|entry| basename(entry).starts_with("libobs_glcapture"));
    let mut layers: Vec<&str> = decoded[2]
        .split(':')
        .filter(|entry| {
            !entry.is_empty()
                && *entry != VULKAN_LAYER
                && (!capture || Some(*entry) != integration.obs_layer)
        })
        .collect();
    // Vulkan's first layer is closest to the game. OBS must copy before our draw,
    // just as its GL preload must run before the deferred native-swap compositor.
    layers.insert(0, VULKAN_LAYER);
    if capture && let Some(obs) = integration.obs_layer {
        layers.insert(0, obs);
    }
    let libraries = libraries.join(":");
    let directories = directories.join(":");
    let layers = layers.join(":");
    let values = [
        libraries.as_str(),
        if path_index == 1 {
            directories.as_str()
        } else {
            decoded[1].as_ref()
        },
        layers.as_str(),
        if path_index == 3 {
            directories.as_str()
        } else {
            decoded[3].as_ref()
        },
    ];
    let mut assignments = String::new();
    for (position, (value, key)) in values.iter().zip(ENVIRONMENT).enumerate() {
        if position == 3 && value.is_empty() {
            continue;
        }
        if !assignments.is_empty() {
            assignments.push(' ');
        }
        assignments.push_str(&quoted_assignment(&format!("{key}={value}")));
    }
    let insertion = last_env.unwrap_or(index);
    let mut output = String::with_capacity(command.len() + assignments.len() + 5);
    for (position, token) in tokens.iter().enumerate() {
        if position == insertion {
            if !output.is_empty() {
                output.push(' ');
            }
            if last_env.is_none() {
                output.push_str("env ");
            }
            output.push_str(&assignments);
        }
        if !remove.contains(&position) {
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(&command[token.raw.clone()]);
        }
    }
    Ok(Some(output))
}

fn quoted_assignment(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            output.push_str("\\\\");
        }
        if character == '\\' {
            output.push('\\');
        }
        if character == '%' {
            output.push('%');
        }
        output.push(character);
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integration(inherited: Option<&str>) -> Integration<'_> {
        Integration {
            library: "/cache/current.so",
            lookup: "/cache/lookup.so",
            layer_directory: "/cache/vulkan",
            obs_layer: Some("VK_LAYER_OBS_vkcapture_64"),
            inherited: [inherited, None, None, None],
        }
    }

    fn preload(command: &str) -> String {
        tokens(command)
            .unwrap()
            .into_iter()
            .filter_map(|token| token.value.strip_prefix("LD_PRELOAD=").map(str::to_owned))
            .next_back()
            .unwrap()
    }

    #[test]
    fn duplicate_preload_assignments_keep_effective_obs_and_replace_old_compositor() {
        let old = format!("$HOME/.cache/vosulay/overlay/{}.so", "a".repeat(64));
        let command = format!(
            "env LD_PRELOAD=ignored.so LD_PRELOAD=\"{old}:/usr/lib/obs_glcapture/libobs_glcapture.so:custom.so\" OBS_VKCAPTURE=1 osu %u"
        );
        let updated = rewrite_exec(&command, &integration(None)).unwrap().unwrap();
        assert_eq!(
            preload(&updated),
            "/cache/lookup.so:/usr/lib/obs_glcapture/libobs_glcapture.so:custom.so:/cache/current.so"
        );
        let arguments = tokens(&updated).unwrap();
        assert_eq!(
            arguments
                .iter()
                .filter(|token| token.value.starts_with("LD_PRELOAD="))
                .count(),
            1
        );
        assert_eq!(arguments.last().unwrap().value, "%u");
        assert!(
            arguments
                .iter()
                .any(|token| token.value == "OBS_VKCAPTURE=1")
        );
    }

    #[test]
    fn environment_reset_and_nested_wrappers_do_not_override_new_preload() {
        let updated = rewrite_exec(
            "env LD_PRELOAD=old.so gamemoderun env -i -u LD_PRELOAD OBS_VKCAPTURE=1 osu %U",
            &integration(Some("inherited.so")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(preload(&updated), "/cache/lookup.so:/cache/current.so");
        let arguments = tokens(&updated).unwrap();
        let reset = arguments
            .iter()
            .position(|token| token.value == "-i")
            .unwrap();
        let assignment = arguments
            .iter()
            .position(|token| token.value.starts_with("LD_PRELOAD="))
            .unwrap();
        assert!(assignment > reset);
        assert_eq!(arguments.last().unwrap().value, "%U");
    }

    #[test]
    fn game_launchers_are_distinct_from_overlay_helpers_and_sandboxes() {
        for command in [
            "vosulay",
            "tosu-overlay",
            "osu-pack-creator",
            "flatpak run sh.ppy.osu",
            "sh -c 'osu'",
            "wine creator.exe",
            "osu-wine --help",
        ] {
            assert!(
                rewrite_exec(command, &integration(None)).unwrap().is_none(),
                "{command}"
            );
        }
        for command in [
            "osu %u",
            "\"/opt/ゲーム/osu.AppImage\" %f",
            "gamescope -f -- osu!",
            "wine \"C:\\\\games\\\\osu!.exe\"",
            "osu-wine %U",
            "osu-wine --devserver akatsuki.gg %u",
        ] {
            assert!(
                rewrite_exec(command, &integration(None)).unwrap().is_some(),
                "{command}"
            );
        }
    }

    #[test]
    fn explicit_layer_path_override_keeps_the_compositor_discoverable() {
        let updated = rewrite_exec(
            "env VK_LAYER_PATH=/explicit VK_ADD_LAYER_PATH=/additional osu",
            &integration(None),
        )
        .unwrap()
        .unwrap();
        let arguments = tokens(&updated).unwrap();
        assert!(
            arguments
                .iter()
                .any(|token| token.value == "VK_LAYER_PATH=/explicit:/cache/vulkan")
        );
        assert!(
            arguments
                .iter()
                .any(|token| token.value == "VK_ADD_LAYER_PATH=/additional")
        );
    }

    #[test]
    fn capture_layer_order_preserves_foreign_settings_and_is_stable() {
        let command = "env LD_PRELOAD=/usr/lib/libobs_glcapture.so:custom.so VK_ADD_LAYER_PATH=/custom/layers VK_INSTANCE_LAYERS=VK_LAYER_MANGOHUD_overlay:VK_LAYER_VOSULAY_overlay:VK_LAYER_OBS_vkcapture_64 OBS_VKCAPTURE=1 osu %u";
        let settings = integration(None);
        let updated = rewrite_exec(command, &settings).unwrap().unwrap();
        assert_eq!(
            preload(&updated),
            "/cache/lookup.so:/usr/lib/libobs_glcapture.so:custom.so:/cache/current.so"
        );
        let arguments = tokens(&updated).unwrap();
        assert!(arguments.iter().any(|token| token.value == "VK_INSTANCE_LAYERS=VK_LAYER_OBS_vkcapture_64:VK_LAYER_VOSULAY_overlay:VK_LAYER_MANGOHUD_overlay"));
        assert!(
            arguments
                .iter()
                .any(|token| token.value == "VK_ADD_LAYER_PATH=/custom/layers:/cache/vulkan")
        );
        assert_eq!(rewrite_exec(&updated, &settings).unwrap().unwrap(), updated);
    }

    #[test]
    fn user_masks_and_overrides_win_without_modifying_system_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let user = temporary.path().join("user");
        let system = temporary.path().join("system");
        fs::create_dir_all(user.join("applications")).unwrap();
        fs::create_dir_all(system.join("applications/nested")).unwrap();
        let entry = "[Desktop Entry]\nType=Application\nName=osu!\nExec=osu %u\n";
        let mask = "[Desktop Entry]\nHidden=true\n";
        fs::write(user.join("applications/osu.desktop"), mask).unwrap();
        fs::write(system.join("applications/osu.desktop"), entry).unwrap();
        fs::write(system.join("applications/nested/lazer.desktop"), entry).unwrap();
        let library = temporary.path().join("current.so");
        fs::write(&library, b"library").unwrap();
        let lookup = temporary.path().join("lookup.so");
        fs::write(&lookup, b"lookup").unwrap();
        assert_eq!(
            refresh_in(
                &user,
                std::slice::from_ref(&system),
                &library,
                &lookup,
                [None; 4]
            )
            .unwrap(),
            1
        );
        assert_eq!(
            fs::read_to_string(user.join("applications/osu.desktop")).unwrap(),
            mask
        );
        assert_eq!(
            fs::read_to_string(system.join("applications/nested/lazer.desktop")).unwrap(),
            entry
        );
        let created = fs::read_to_string(user.join("applications/nested-lazer.desktop")).unwrap();
        assert!(
            preload(
                created
                    .lines()
                    .find_map(|line| line.strip_prefix("Exec="))
                    .unwrap()
            )
            .ends_with("current.so")
        );
        assert_eq!(
            refresh_in(&user, &[system], &library, &lookup, [None; 4]).unwrap(),
            0
        );
    }
}
