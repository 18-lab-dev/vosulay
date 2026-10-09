use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn register() {
    if let Err(error) = ensure_entry() {
        eprintln!("Vosulay: could not register app launcher: {error:#}");
    }
}

#[cfg(target_os = "linux")]
fn ensure_entry() -> Result<()> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.data_dir().to_owned()))
        .context("could not find user data directory")?;
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let system_dirs = std::env::split_paths(&data_dirs).collect::<Vec<_>>();
    let executable = std::env::var_os("VOSULAY_LAUNCHER_EXECUTABLE")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    linux::ensure_entry(&data_home, &system_dirs, &executable)
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, ensure};
    use std::{
        fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    const ENTRY_NAME: &str = "com.vosu.vosulay.desktop";
    const ICON: &[u8] = include_bytes!("../../assets/branding/vosulay-app-icon.svg");

    pub(super) fn ensure_entry(
        data_home: &Path,
        system_dirs: &[PathBuf],
        executable: &Path,
    ) -> Result<()> {
        if system_dirs
            .iter()
            .any(|dir| dir.join("applications").join(ENTRY_NAME).is_file())
        {
            return Ok(());
        }
        let exec = quoted_exec(executable)?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Vosulay\nGenericName=In-game overlay for vosu and tosu counters\nComment=In-game overlay for vosu and tosu counters\nExec={exec}\nIcon=com.vosu.vosulay\nTerminal=false\nCategories=Game;Utility;\nStartupNotify=false\nKeywords=osu;vosu;tosu;overlay;counters;\n"
        );
        write_if_changed(
            &data_home.join("icons/hicolor/scalable/apps/com.vosu.vosulay.svg"),
            ICON,
        )?;
        write_if_changed(
            &data_home.join("applications").join(ENTRY_NAME),
            entry.as_bytes(),
        )
    }

    fn quoted_exec(path: &Path) -> Result<String> {
        ensure!(
            path.is_absolute(),
            "launcher executable path must be absolute"
        );
        let path = path
            .to_str()
            .context("launcher executable path is not UTF-8")?;
        let mut exec = String::with_capacity(path.len() + 2);
        exec.push('"');
        for character in path.chars() {
            match character {
                // Desktop Entry string escaping is applied after Exec quoting.
                '\\' => exec.push_str("\\\\\\\\"),
                '"' | '`' | '$' => {
                    exec.push_str("\\\\");
                    exec.push(character);
                }
                '%' => exec.push_str("%%"),
                '\n' => exec.push_str("\\n"),
                '\r' => exec.push_str("\\r"),
                '\t' => exec.push_str("\\t"),
                _ => exec.push(character),
            }
        }
        exec.push('"');
        Ok(exec)
    }

    fn write_if_changed(path: &Path, content: &[u8]) -> Result<()> {
        match fs::read(path) {
            Ok(existing) if existing == content => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = path
            .parent()
            .context("launcher file has no parent directory")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(content)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        temporary.persist(path)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::MetadataExt;

        #[test]
        fn content_and_idempotence() {
            let directory = tempfile::tempdir().unwrap();
            let exe = Path::new("/opt/My Vosulay/vosulay");
            ensure_entry(directory.path(), &[], exe).unwrap();
            let path = directory.path().join("applications").join(ENTRY_NAME);
            let entry = fs::read_to_string(&path).unwrap();
            assert!(entry.contains("Exec=\"/opt/My Vosulay/vosulay\"\n"));
            assert!(entry.contains("Comment=In-game overlay for vosu and tosu counters\n"));
            assert!(entry.contains("Icon=com.vosu.vosulay\n"));
            assert!(
                entry.contains("Terminal=false\nCategories=Game;Utility;\nStartupNotify=false\n")
            );
            let icon = directory
                .path()
                .join("icons/hicolor/scalable/apps/com.vosu.vosulay.svg");
            assert_eq!(fs::read(&icon).unwrap(), ICON);
            let entry_before = fs::metadata(&path).unwrap();
            let icon_before = fs::metadata(&icon).unwrap();
            ensure_entry(directory.path(), &[], exe).unwrap();
            assert_eq!(entry_before.ino(), fs::metadata(&path).unwrap().ino());
            assert_eq!(icon_before.ino(), fs::metadata(&icon).unwrap().ino());
            ensure_entry(directory.path(), &[], Path::new("/opt/updated/vosulay")).unwrap();
            assert!(
                fs::read_to_string(path)
                    .unwrap()
                    .contains("Exec=\"/opt/updated/vosulay\"\n")
            );
        }

        #[test]
        fn system_entry_skips_all_user_files() {
            let directory = tempfile::tempdir().unwrap();
            let system = directory.path().join("system");
            fs::create_dir_all(system.join("applications")).unwrap();
            fs::write(
                system.join("applications").join(ENTRY_NAME),
                b"system entry",
            )
            .unwrap();
            let user = directory.path().join("user");
            ensure_entry(
                &user,
                &[directory.path().join("absent"), system],
                Path::new("/opt/vosulay"),
            )
            .unwrap();
            assert!(!user.exists());
        }

        #[test]
        fn exec_quotes_reserved_characters_and_field_codes() {
            assert_eq!(
                quoted_exec(Path::new("/a b/\"$`\\%/vosulay")).unwrap(),
                "\"/a b/\\\\\"\\\\$\\\\`\\\\\\\\%%/vosulay\""
            );
            assert!(quoted_exec(Path::new("relative/vosulay")).is_err());
        }
    }
}

#[cfg(windows)]
fn ensure_entry() -> Result<()> {
    use std::{fs, os::windows::ffi::OsStrExt};
    use windows::{
        Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IPersistFile, STGM_READ,
            },
            UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink},
        },
        core::{Interface, PCWSTR, w},
    };

    struct ComApartment(bool);
    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if initialized != RPC_E_CHANGED_MODE {
        initialized.ok()?;
    }
    let _apartment = ComApartment(initialized.is_ok());
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .context("launcher executable has no parent")?;
    let programs = PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is not set")?)
        .join("Microsoft/Windows/Start Menu/Programs");
    let shortcut = programs.join("Vosulay.lnk");
    let wide = |path: &std::path::Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let target = wide(&executable);
    let working_directory = wide(directory);
    let shortcut_path = wide(&shortcut);
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    let file: IPersistFile = link.cast()?;
    if shortcut.is_file() && unsafe { file.Load(PCWSTR(shortcut_path.as_ptr()), STGM_READ) }.is_ok()
    {
        let mut existing = vec![0u16; 32768];
        if unsafe { link.GetPath(&mut existing, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32) }
            .is_ok()
        {
            let length = existing
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(existing.len());
            if existing[..length] == target[..target.len() - 1] {
                return Ok(());
            }
        }
    }
    fs::create_dir_all(programs)?;
    unsafe {
        link.SetPath(PCWSTR(target.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(working_directory.as_ptr()))?;
        link.SetIconLocation(PCWSTR(target.as_ptr()), 0)?;
        link.SetDescription(w!("In-game overlay for vosu and tosu counters"))?;
        file.Save(PCWSTR(shortcut_path.as_ptr()), true)?;
    }
    Ok(())
}
