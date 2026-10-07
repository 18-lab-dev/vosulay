//! Owned Win32 tray window. Its modal menu runs only on this dedicated thread.
use std::{
    cell::Cell,
    io::Cursor,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use vosulay_frame::PlatformEvent;
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
            DeleteObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_SPACE,
            },
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETFOCUS,
                NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

const CALLBACK: u32 = WM_APP + 1;
const STOP: u32 = WM_APP + 2;
const HOTKEY_ID: i32 = 1;
const EDIT: u8 = 1;
const HIDE: u8 = 2;
const RELOAD: u8 = 4;
const EXIT: u8 = 8;
const SHORTCUT_EDIT: u8 = 16;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_CLASS: AtomicUsize = AtomicUsize::new(1);

struct Shared {
    hwnd: AtomicUsize,
    stopping: AtomicBool,
    // One atomic snapshot keeps both checkmarks consistent.
    state: AtomicU8,
    overflow: AtomicU8,
    host: thread::Thread,
}

pub(crate) struct Tray {
    shared: Arc<Shared>,
    events: Receiver<PlatformEvent>,
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Tray {
    pub(crate) fn start() -> Result<Self> {
        let shared = Arc::new(Shared {
            hwnd: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            state: AtomicU8::new(2), // Initially visible, not editing.
            overflow: AtomicU8::new(0),
            host: thread::current(),
        });
        let (tx, events) = mpsc::sync_channel(16);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("vosulay-tray".into())
            .spawn(move || {
                match WindowOwner::create(worker_shared, tx) {
                    Ok(owner) => {
                        let _ = ready_tx.send(Ok(()));
                        owner.run();
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
                let _ = done_tx.send(());
            })
            .context("starting native tray thread")?;
        let mut tray = Self {
            shared,
            events,
            done,
            thread: Some(thread),
        };
        match ready.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(tray),
            Ok(Err(error)) => {
                let _ = tray.shutdown();
                Err(error)
            }
            Err(error) => {
                let _ = tray.shutdown();
                Err(anyhow!(
                    "native tray initialization did not complete: {error}"
                ))
            }
        }
    }

    pub(crate) fn poll(&self) -> Option<PlatformEvent> {
        if let Ok(event) = self.events.try_recv() {
            return Some(event);
        }
        // If a caller temporarily stops polling, preserve Exit/Reload and the net
        // parity of toggles without blocking the message thread or growing a queue.
        for bit in [EDIT, SHORTCUT_EDIT, HIDE, RELOAD, EXIT] {
            if self.shared.overflow.fetch_and(!bit, Ordering::AcqRel) & bit != 0 {
                return Some(event_for(bit));
            }
        }
        None
    }

    pub(crate) fn set_state(&self, editing: bool, visible: bool) {
        self.shared.state.store(
            u8::from(editing) | (u8::from(visible) << 1),
            Ordering::Release,
        );
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        if self.thread.is_none() {
            return Ok(());
        }
        self.shared.stopping.store(true, Ordering::Release);
        let raw = self.shared.hwnd.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let hwnd = HWND(raw as *mut _);
                // Cancel an active TrackPopupMenu loop, then ask the owner to quit.
                let _ = PostMessageW(Some(hwnd), WM_CANCELMODE, WPARAM(0), LPARAM(0));
                let _ = PostMessageW(Some(hwnd), STOP, WPARAM(0), LPARAM(0));
            }
        }
        if matches!(
            self.done.recv_timeout(SHUTDOWN_TIMEOUT),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            // Dropping JoinHandle detaches rather than hanging the caller. The
            // stop flag remains live and the owner cleans up when it can resume.
            self.thread.take();
            bail!("native tray thread did not stop within two seconds");
        }
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow!("native tray thread panicked"))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn event_for(bit: u8) -> PlatformEvent {
    match bit {
        EDIT => PlatformEvent::ToggleEdit,
        SHORTCUT_EDIT => PlatformEvent::ShortcutToggleEdit,
        HIDE => PlatformEvent::ToggleVisibility,
        RELOAD => PlatformEvent::Reload,
        _ => PlatformEvent::Exit,
    }
}

struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

struct WindowState {
    shared: Arc<Shared>,
    events: SyncSender<PlatformEvent>,
    icon: OwnedIcon,
    taskbar_created: u32,
    added: Cell<bool>,
    version4: Cell<bool>,
    menu_open: Cell<bool>,
}

impl WindowState {
    fn notify_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: CALLBACK,
            hIcon: self.icon.0,
            ..Default::default()
        };
        for (dst, src) in data.szTip.iter_mut().zip("Vosulay".encode_utf16()) {
            *dst = src;
        }
        data
    }

    unsafe fn add(&self, hwnd: HWND) -> Result<()> {
        let mut data = self.notify_data(hwnd);
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            bail!("Shell_NotifyIconW could not add the Vosulay tray icon");
        }
        self.added.set(true);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        self.version4
            .set(unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool());
        Ok(())
    }

    fn emit(&self, bit: u8) {
        if let Err(TrySendError::Full(_)) = self.events.try_send(event_for(bit)) {
            if matches!(bit, EDIT | SHORTCUT_EDIT | HIDE) {
                self.shared.overflow.fetch_xor(bit, Ordering::AcqRel);
            } else {
                self.shared.overflow.fetch_or(bit, Ordering::AcqRel);
            }
        }
        self.shared.host.unpark();
    }

    unsafe fn menu(&self, hwnd: HWND, mut point: POINT) -> Result<()> {
        if self.menu_open.replace(true) {
            return Ok(());
        }
        struct MenuGuard<'a> {
            menu: HMENU,
            open: &'a Cell<bool>,
        }
        impl Drop for MenuGuard<'_> {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyMenu(self.menu);
                }
                self.open.set(false);
            }
        }
        let menu = match unsafe { CreatePopupMenu() } {
            Ok(menu) => menu,
            Err(error) => {
                self.menu_open.set(false);
                return Err(error.into());
            }
        };
        let _guard = MenuGuard {
            menu,
            open: &self.menu_open,
        };
        let state = self.shared.state.load(Ordering::Acquire);
        unsafe {
            AppendMenuW(
                menu,
                MF_STRING
                    | if state & 1 != 0 {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    },
                EDIT as usize,
                w!("Edit overlays"),
            )?;
            AppendMenuW(
                menu,
                MF_STRING
                    | if state & 2 == 0 {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    },
                HIDE as usize,
                w!("Hide overlays"),
            )?;
            AppendMenuW(menu, MF_STRING, RELOAD as usize, w!("Reload overlays"))?;
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
            AppendMenuW(menu, MF_STRING, EXIT as usize, w!("Exit"))?;
            if point.x == -1 && point.y == -1 {
                GetCursorPos(&mut point)?;
            }
            // Required by the standard tray-menu dismissal protocol. This is
            // the only focus-changing operation, in response to explicit input.
            let _ = SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            )
            .0 as u8;
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            if !self.shared.stopping.load(Ordering::Acquire) {
                if matches!(command, EDIT | HIDE | RELOAD | EXIT) {
                    self.emit(command);
                }
                let _ = Shell_NotifyIconW(NIM_SETFOCUS, &self.notify_data(hwnd));
            }
        }
        Ok(())
    }
}

struct WindowOwner {
    state: Box<WindowState>,
    hwnd: HWND,
    class: Vec<u16>,
    instance: HINSTANCE,
    hotkey_registered: bool,
}

impl WindowOwner {
    fn create(shared: Arc<Shared>, events: SyncSender<PlatformEvent>) -> Result<Self> {
        unsafe {
            let icon = branding_icon()?;
            let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
            let class: Vec<u16> = format!(
                "VosulayTray-{}-{}",
                std::process::id(),
                NEXT_CLASS.fetch_add(1, Ordering::Relaxed)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let definition = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&definition) == 0 {
                return Err(windows::core::Error::from_thread().into());
            }
            let mut owner = Self {
                state: Box::new(WindowState {
                    shared,
                    events,
                    icon,
                    taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                    added: Cell::new(false),
                    version4: Cell::new(false),
                    menu_open: Cell::new(false),
                }),
                hwnd: HWND::default(),
                class,
                instance,
                hotkey_registered: false,
            };
            // A hidden top-level window, not HWND_MESSAGE: Explorer's registered
            // TaskbarCreated broadcast must reach this window after a restart.
            owner.hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(owner.class.as_ptr()),
                w!("Vosulay"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                Some((&*owner.state as *const WindowState).cast()),
            )?;
            owner
                .state
                .shared
                .hwnd
                .store(owner.hwnd.0 as usize, Ordering::Release);
            RegisterHotKey(
                Some(owner.hwnd),
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                VK_SPACE.0 as u32,
            )
            .context("registering Ctrl+Shift+Space; another application may own this shortcut")?;
            owner.hotkey_registered = true;
            if !owner.state.shared.stopping.load(Ordering::Acquire) {
                owner.state.add(owner.hwnd)?;
            }
            Ok(owner)
        }
    }

    fn run(&self) {
        unsafe {
            let mut message = MSG::default();
            while !self.state.shared.stopping.load(Ordering::Acquire) {
                let status = GetMessageW(&mut message, None, 0, 0).0;
                if status <= 0 {
                    if status < 0 {
                        let _ = self.state.events.try_send(PlatformEvent::Warning(format!(
                            "native tray message loop failed: {}",
                            windows::core::Error::from_thread()
                        )));
                        self.state.shared.host.unpark();
                    }
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

impl Drop for WindowOwner {
    fn drop(&mut self) {
        self.state.shared.hwnd.store(0, Ordering::Release);
        unsafe {
            if self.state.added.replace(false) {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.state.notify_data(self.hwnd));
            }
            if self.hotkey_registered {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
            if !self.hwnd.is_invalid() {
                let _ = DestroyWindow(self.hwnd);
            }
            let _ = UnregisterClassW(PCWSTR(self.class.as_ptr()), Some(self.instance));
        }
        // The icon is destroyed after the shell entry and HWND, by OwnedIcon.
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
        if !pointer.is_null() {
            let state = &*pointer;
            if message == STOP || message == WM_CLOSE {
                state.shared.stopping.store(true, Ordering::Release);
                if state.menu_open.get() {
                    let _ = EndMenu();
                }
                PostQuitMessage(0);
                return LRESULT(0);
            }
            if message == WM_HOTKEY && wparam.0 == HOTKEY_ID as usize {
                if !state.shared.stopping.load(Ordering::Acquire) {
                    state.emit(SHORTCUT_EDIT);
                }
                return LRESULT(0);
            }
            if message == WM_NCDESTROY {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            } else if message == state.taskbar_created && state.taskbar_created != 0 {
                state.added.set(false);
                if !state.shared.stopping.load(Ordering::Acquire)
                    && let Err(error) = state.add(hwnd)
                {
                    let _ = state
                        .events
                        .try_send(PlatformEvent::Warning(error.to_string()));
                    state.shared.host.unpark();
                }
                return LRESULT(0);
            } else if message == CALLBACK && !state.shared.stopping.load(Ordering::Acquire) {
                let notification = if state.version4.get() {
                    (lparam.0 as u32) & 0xffff
                } else {
                    lparam.0 as u32
                };
                if notification == WM_CONTEXTMENU
                    || (!state.version4.get() && notification == WM_RBUTTONUP)
                {
                    let point = if state.version4.get() {
                        POINT {
                            x: wparam.0 as u16 as i16 as i32,
                            y: (wparam.0 >> 16) as u16 as i16 as i32,
                        }
                    } else {
                        POINT { x: -1, y: -1 }
                    };
                    if let Err(error) = state.menu(hwnd, point) {
                        let _ = state.events.try_send(PlatformEvent::Warning(format!(
                            "native tray menu failed: {error}"
                        )));
                        state.shared.host.unpark();
                    }
                }
                return LRESULT(0);
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn branding_icon() -> Result<OwnedIcon> {
    let mut decoder = png::Decoder::new(Cursor::new(include_bytes!(
        "../../../assets/branding/vosulay-app-icon.png"
    )));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .context("decoding Vosulay tray branding")?;
    let size = reader
        .output_buffer_size()
        .context("Vosulay icon dimensions overflow")?;
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels)?;
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => bail!("Vosulay icon palette was not expanded"),
    };
    let width = i32::try_from(info.width)?;
    let height = i32::try_from(info.height)?;
    if width <= 0 || height <= 0 {
        bail!("Vosulay icon is empty");
    }
    unsafe {
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let mask_stride = (width as usize).div_ceil(16) * 2;
        let mask_pixels = vec![0u8; mask_stride * height as usize];
        let mask = CreateBitmap(width, height, 1, 1, Some(mask_pixels.as_ptr().cast()));
        if mask.is_invalid() {
            let error = windows::core::Error::from_thread();
            let _ = DeleteObject(color.into());
            return Err(error.into());
        }
        let output =
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), width as usize * height as usize * 4);
        for (source, target) in pixels[..info.buffer_size()]
            .chunks_exact(channels)
            .zip(output.chunks_exact_mut(4))
        {
            let (r, g, b, a) = match channels {
                4 => (source[0], source[1], source[2], source[3]),
                3 => (source[0], source[1], source[2], 255),
                2 => (source[0], source[0], source[0], source[1]),
                _ => (source[0], source[0], source[0], 255),
            };
            // Alpha icons use a premultiplied BGRA DIB and a zero AND mask.
            target.copy_from_slice(&[
                ((b as u16 * a as u16 + 127) / 255) as u8,
                ((g as u16 * a as u16 + 127) / 255) as u8,
                ((r as u16 * a as u16 + 127) / 255) as u8,
                a,
            ]);
        }
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        });
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        Ok(OwnedIcon(icon?))
    }
}
