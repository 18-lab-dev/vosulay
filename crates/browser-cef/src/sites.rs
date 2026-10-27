//! A new site starts at an inert document. Its renderer acknowledges registration
//! before the real overlay loads. Grants are process-local, keyed by the iframe's
//! fresh request token and exact origin; a new process must prepare again.
use cef::*;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
};
use url::Url;

pub(crate) const REGISTER: &str = "vosulay.site.register";
pub(crate) const ACK: &str = "vosulay.site.ack";
pub(crate) const ACTIVE: &str = "vosulay.site.active";
pub(crate) const CHECK: &str = "vosulay.site.check";
pub(crate) const REVOKE: &str = "vosulay.site.revoke";
pub(crate) const PREPARE: &str = "/__vosulay_prepare";
pub(crate) const NAME_PREFIX: &str = "vosulay-site-";

struct Entry {
    origin: String,
    frame: Option<Frame>,
    process: Option<String>,
}
#[derive(Clone, Default)]
pub(crate) struct Sites(Rc<RefCell<State>>);
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    events: VecDeque<(String, bool, String)>,
}
impl Sites {
    pub(crate) fn authorize(&self, request: &str, origin: &str) -> bool {
        if request.is_empty() || request.len() > 128 {
            return false;
        }
        let Ok(url) = Url::parse(origin) else {
            return false;
        };
        if url.scheme() != "http"
            || !matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || url.origin().ascii_serialization() != origin
        {
            return false;
        }
        self.0.borrow_mut().entries.insert(
            request.to_owned(),
            Entry {
                origin: origin.to_owned(),
                frame: None,
                process: None,
            },
        );
        true
    }
    pub(crate) fn release(&self, request: &str) {
        if let Some(entry) = self.0.borrow_mut().entries.remove(request)
            && let Some(frame) = entry.frame
        {
            send(&frame, REVOKE, &[request]);
        }
    }
    pub(crate) fn clear(&self) {
        let mut state = self.0.borrow_mut();
        for (request, entry) in state.entries.drain() {
            if let Some(frame) = entry.frame {
                send(&frame, REVOKE, &[&request]);
            }
        }
        state.events.clear();
    }
    pub(crate) fn next(&self) -> Option<(String, bool, String)> {
        self.0.borrow_mut().events.pop_front()
    }
    pub(crate) fn loaded(&self, frame: &Frame) {
        let Some(name) = crate::strings::decode(&frame.name(), 256) else {
            return;
        };
        let Some(request) = name.strip_prefix(NAME_PREFIX) else {
            return;
        };
        let Some(url) =
            crate::strings::decode(&frame.url(), 8192).and_then(|u| Url::parse(&u).ok())
        else {
            return;
        };
        let Some(id) = crate::strings::decode(&frame.identifier(), 256) else {
            return;
        };
        let mut state = self.0.borrow_mut();
        let Some(entry) = state.entries.get_mut(request) else {
            return;
        };
        if url.origin().ascii_serialization() != entry.origin {
            return;
        }
        if url.path() == PREPARE {
            entry.frame = Some(frame.clone());
            send(frame, REGISTER, &[request, &entry.origin, &id]);
        } else {
            // Load callbacks and renderer IPC use different delivery paths.
            // Ask the loaded renderer explicitly; do not race an earlier context ACK.
            send(frame, CHECK, &[request, &entry.origin, &id]);
        }
    }
    pub(crate) fn dispatch(
        &self,
        frame: Option<&mut Frame>,
        source: ProcessId,
        message: Option<&mut ProcessMessage>,
    ) -> i32 {
        let Some(message) = message else { return 0 };
        let prepare = crate::strings::equals(&message.name(), ACK);
        if !prepare && !crate::strings::equals(&message.name(), ACTIVE) {
            return 0;
        }
        if source != ProcessId::RENDERER {
            return 1;
        }
        let Some(frame) = frame else { return 1 };
        let Some(values) = values(message, if prepare { 4 } else { 5 }) else {
            return 1;
        };
        let (request, origin, id) = (&values[0], &values[1], &values[2]);
        if !crate::strings::equals(&frame.identifier(), id) {
            return 1;
        }
        let Some(url) =
            crate::strings::decode(&frame.url(), 8192).and_then(|u| Url::parse(&u).ok())
        else {
            return 1;
        };
        let mut state = self.0.borrow_mut();
        let Some(entry) = state.entries.get_mut(request) else {
            return 1;
        };
        if &entry.origin != origin || url.origin().ascii_serialization() != *origin {
            return 1;
        }
        if prepare {
            if url.path() != PREPARE
                || !entry
                    .frame
                    .as_ref()
                    .is_some_and(|registered| crate::strings::equals(&registered.identifier(), id))
            {
                return 1;
            }
            entry.process = Some(values[3].clone());
            if values[3].len() != 64 || !values[3].bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return 1;
            }
            state
                .events
                .push_back((request.clone(), true, values[3].clone()));
        } else if values[4] == "1" && entry.process.as_deref() == Some(values[3].as_str()) {
            entry.frame = Some(frame.clone());
        } else {
            state
                .events
                .push_back((request.clone(), false, values[3].clone()));
        }
        1
    }
}
pub(crate) fn values(message: &ProcessMessage, count: usize) -> Option<Vec<String>> {
    let args = message.argument_list()?;
    if args.size() != count {
        return None;
    }
    (0..count)
        .map(|i| {
            (args.get_type(i) == ValueType::STRING)
                .then(|| crate::strings::decode(&args.string(i), 8192))
                .flatten()
        })
        .collect()
}
pub(crate) fn send(frame: &Frame, name: &str, values: &[&str]) {
    let Some(mut message) = process_message_create(Some(&name.into())) else {
        return;
    };
    let Some(args) = message.argument_list() else {
        return;
    };
    for (i, value) in values.iter().enumerate() {
        if args.set_string(i, Some(&(*value).into())) == 0 {
            return;
        }
    }
    frame.send_process_message(
        if name == ACK || name == ACTIVE {
            ProcessId::BROWSER
        } else {
            ProcessId::RENDERER
        },
        Some(&mut message),
    );
}
wrap_load_handler! {
    pub(crate) struct SiteLoading { sites: Sites }
    impl LoadHandler {
        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            if let Some(frame) = frame { self.sites.loaded(frame); }
        }
    }
}
