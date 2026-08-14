//! Send a changing checkerboard through the certified SHM protocol.
//! Usage: shm-checkerboard <target-pid> [credential-file [expected-width expected-height [opaque-marker]]]
use overlay_host::{Host, HostEvent};
use overlay_protocol::TextureDescriptor;
use std::os::fd::OwnedFd;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid = args.next().ok_or("target pid required")?.parse()?;
    let credential_file = args.next();
    let expected = match (args.next(), args.next()) {
        (Some(width), Some(height)) => Some((width.parse::<u32>()?, height.parse::<u32>()?)),
        (None, None) => None,
        _ => return Err("expected width and height must be provided together".into()),
    };
    let opaque_marker = match args.next().as_deref() {
        None => false,
        Some("opaque-marker") => true,
        Some(_) => return Err("unknown checkerboard mode".into()),
    };
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let host = Host::new(pid)?;
    if let Some(path) = credential_file {
        let mut payload = host.endpoint().name.clone();
        payload.push(0);
        payload.extend_from_slice(hex(&host.cookie()).as_bytes());
        payload.push(0);
        payload.extend_from_slice(std::process::id().to_string().as_bytes());
        let temporary = format!("{path}.{}", std::process::id());
        std::fs::write(&temporary, payload)?;
        std::fs::rename(temporary, path)?;
    }
    eprintln!(
        "agent endpoint={} cookie={}",
        hex(host.endpoint().name.as_slice()),
        hex(&host.cookie())
    );
    let mut session = host.accept()?;
    let target = loop {
        match session.recv()? {
            HostEvent::TargetCreated(target) => break target,
            HostEvent::Disconnected => {
                return Err("agent disconnected before advertising a target".into());
            }
            _ => {}
        }
    };
    if expected.is_some_and(|dimensions| dimensions != (target.width, target.height)) {
        return Err(format!(
            "unexpected target dimensions {}x{}",
            target.width, target.height
        )
        .into());
    }
    let target_id = target.target_id;
    let width = target.width as usize;
    let height = target.height as usize;
    let bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("target dimensions overflow")?;
    let offset = 17usize;
    let allocation = offset.checked_add(bytes).ok_or("allocation overflow")?;
    let fd = unsafe { libc::memfd_create(c"vosulay-checkerboard".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), allocation as libc::off_t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            allocation,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().into());
    }
    let pixels = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>().add(offset), bytes) };
    for tick in 0u8.. {
        for y in 0..height {
            for x in 0..width {
                let color = if opaque_marker {
                    &[0x4d, 0xc3, 0x7a, 0xff]
                } else if (((x / 32) ^ (y / 32)) as u8 ^ tick) & 1 == 0 {
                    &[0x22, 0xaa, 0xff, 0xd0]
                } else {
                    &[0xff, 0x44, 0x88, 0x90]
                };
                let p = (y * width + x) * 4;
                pixels[p..p + 4].copy_from_slice(color);
            }
        }
        let descriptor = TextureDescriptor {
            target_id,
            width: target.width,
            height: target.height,
            stride: target.width * 4,
            offset: offset as u64,
            size: allocation as u64,
            format: 0,
        };
        session.send_shm(target_id, descriptor, fd.try_clone()?)?;
        thread::sleep(Duration::from_millis(33));
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
use std::os::fd::{AsRawFd, FromRawFd};
