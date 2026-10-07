use std::path::{Path, PathBuf};

use crate::{config::AppConfig, migration::import_automatically};

// Do not use tosu's settings API: even a GET can migrate/rename its files.
pub(super) fn sources() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let directory = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        if let Some(directory) = directory {
            candidates.push(directory.join("tosu/settings/__ingame__.values.json"));
        }
    }
    #[cfg(windows)]
    candidates.extend(
        process_directories()
            .into_iter()
            .map(|directory| directory.join("settings/__ingame__.values.json")),
    );
    if let Ok(directory) = std::env::current_dir() {
        candidates.push(directory.join("settings/__ingame__.values.json"));
        candidates.push(directory.join("static/__ingame__/settings.values.json"));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("settings/__ingame__.values.json"));
        candidates.push(directory.join("static/__ingame__/settings.values.json"));
    }
    candidates.retain(|path| path.is_file());
    candidates.sort();
    candidates.dedup();
    candidates
}

pub(super) fn migrate(config: &mut AppConfig, path: &Path) {
    if config.tosu_migration_completed {
        return;
    }
    let candidates = sources();
    match candidates.as_slice() {
        [source] => match import_automatically(config, path, source) {
            Ok(true) => eprintln!("Imported tosu layout from {}", source.display()),
            Ok(false) => {}
            Err(error) => eprintln!(
                "Tosu layout import was not applied: {error:#}. Use Settings > Import tosu layout to select a file."
            ),
        },
        [] => {}
        _ => eprintln!(
            "Multiple tosu layouts found; automatic import skipped. Use Settings > Import tosu layout to choose one."
        ),
    }
}

#[cfg(windows)]
fn process_directories() -> Vec<PathBuf> {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                Threading::{
                    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                    QueryFullProcessImageNameW,
                },
            },
        },
        core::PWSTR,
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let snapshot = Handle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut directories = Vec::new();
    let mut present = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while present {
        let length = entry
            .szExeFile
            .iter()
            .position(|&ch| ch == 0)
            .unwrap_or(entry.szExeFile.len());
        let is_tosu = length == b"tosu.exe".len()
            && entry.szExeFile[..length]
                .iter()
                .zip(b"tosu.exe")
                .all(|(&ch, &expected)| {
                    u8::try_from(ch).is_ok_and(|ch| ch.eq_ignore_ascii_case(&expected))
                });
        if is_tosu
            && let Ok(handle) = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    entry.th32ProcessID,
                )
            }
        {
            let handle = Handle(handle);
            let mut path = [0u16; 32768];
            let mut length = path.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    handle.0,
                    PROCESS_NAME_WIN32,
                    PWSTR(path.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok()
                && let Some(directory) =
                    PathBuf::from(String::from_utf16_lossy(&path[..length as usize])).parent()
            {
                directories.push(directory.to_owned());
            }
        }
        present = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    directories
}
