use super::Shared;
use ksni::blocking::TrayMethods;
use ksni::{
    ToolTip, Tray,
    menu::{CheckmarkItem, MenuItem, StandardItem},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use vosulay_frame::PlatformEvent;

pub(super) struct LinuxTray {
    shared: Arc<Shared>,
    pub editing: bool,
    pub visible: bool,
    warned: AtomicBool,
    tool_mode: bool,
}

impl LinuxTray {
    pub fn start(shared: Arc<Shared>) -> Option<ksni::blocking::Handle<Self>> {
        let tray = Self {
            shared: shared.clone(),
            editing: false,
            visible: true,
            warned: AtomicBool::new(false),
            tool_mode: std::env::var("VOSU_TOOL_PROTOCOL").as_deref() == Ok("1"),
        };
        match tray.assume_sni_available(true).spawn() {
            Ok(handle) => Some(handle),
            Err(error) => {
                shared.event(PlatformEvent::Warning(format!("tray unavailable: {error}")));
                None
            }
        }
    }
}

impl Tray for LinuxTray {
    fn id(&self) -> String {
        "com.vosu.vosulay".into()
    }
    fn title(&self) -> String {
        "vosulay".into()
    }
    fn icon_name(&self) -> String {
        "com.vosu.vosulay".into()
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "vosulay".into(),
            icon_name: self.icon_name(),
            ..Default::default()
        }
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        if !self.warned.swap(true, Ordering::Relaxed) {
            self.shared.event(PlatformEvent::Warning(
                "StatusNotifierWatcher unavailable; tray hidden".into(),
            ));
        }
        true
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Edit mode".into(),
                checked: self.editing,
                activate: Box::new(|tray: &mut Self| tray.shared.event(PlatformEvent::ToggleEdit)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Hide vosulay".into(),
                checked: !self.visible,
                activate: Box::new(|tray: &mut Self| {
                    tray.shared.event(PlatformEvent::ToggleVisibility)
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: if self.tool_mode {
                    "Open in vosu dashboard"
                } else {
                    "Exit vosulay"
                }
                .into(),
                activate: Box::new(|tray: &mut Self| {
                    tray.shared.event(if tray.tool_mode {
                        PlatformEvent::OpenDashboard
                    } else {
                        PlatformEvent::Exit
                    })
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
