use anyhow::Result;

#[cfg(target_os = "linux")]
pub struct InstanceGuard {
    _file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl InstanceGuard {
    pub fn runtime_directory() -> Result<std::path::PathBuf> {
        use anyhow::Context;
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.cache_dir().to_owned()))
            .context("could not find a directory for the instance lock")?;
        Ok(base.join("vosulay"))
    }

    pub fn acquire() -> Result<Option<Self>> {
        Self::acquire_at(&Self::runtime_directory()?.join("instance.lock"))
    }

    fn acquire_at(path: &std::path::Path) -> Result<Option<Self>> {
        use anyhow::Context;
        use std::{fs::OpenOptions, os::fd::AsRawFd, os::unix::fs::OpenOptionsExt};
        std::fs::create_dir_all(path.parent().context("instance lock has no parent")?)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error.into());
        }
        Ok(Some(Self { _file: file }))
    }
}

#[cfg(windows)]
pub struct InstanceGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl InstanceGuard {
    pub fn acquire() -> Result<Option<Self>> {
        use windows::{
            Win32::{
                Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError},
                System::Threading::CreateMutexW,
            },
            core::w,
        };
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\vosulay-instance"))? };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle)? };
            return Ok(None);
        }
        Ok(Some(Self { handle }))
    }
}

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
pub fn user_suffix() -> Result<u64> {
    use anyhow::Context;
    // The account's profile directory is stable across sessions. Use fixed
    // FNV-1a rather than RandomState so pipe names agree across launches.
    let directory = directories::BaseDirs::new().context("could not find user profile")?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in directory.home_dir().as_os_str().as_encoded_bytes() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    Ok(hash)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::InstanceGuard;

    #[test]
    fn exclusive_until_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vosulay/instance.lock");
        let first = InstanceGuard::acquire_at(&path).unwrap().unwrap();
        assert!(InstanceGuard::acquire_at(&path).unwrap().is_none());
        drop(first);
        assert!(InstanceGuard::acquire_at(&path).unwrap().is_some());
    }
}
