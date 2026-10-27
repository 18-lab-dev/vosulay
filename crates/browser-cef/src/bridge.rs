use crate::scheme::MANAGER_URL;
use cef::*;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc as Shared,
    sync::{Arc, Mutex},
};
use url::Url;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_PENDING_MESSAGES: usize = 64;
const MESSAGE_NAME: &str = "vosulay.manager.post";
const BACKEND_KEY: &str = "vosulay.backend_url";
const BROKER_KEY: &str = "vosulay.broker_url";
const BROKER_SECRET_KEY: &str = "vosulay.broker_secret";

#[derive(Clone, Default)]
pub struct BridgeInbox(Arc<Mutex<VecDeque<String>>>);

impl BridgeInbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Delegate Client::on_process_message_received here. Unknown messages return
    /// 0; recognized but unauthorized/malformed messages are consumed, not queued.
    pub fn dispatch(
        &self,
        browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        source_process: ProcessId,
        message: Option<&mut ProcessMessage>,
    ) -> i32 {
        let Some(message) = message else { return 0 };
        if !crate::strings::equals(&message.name(), MESSAGE_NAME) {
            return 0;
        }
        if source_process != ProcessId::RENDERER {
            return 1;
        }
        let (Some(browser), Some(frame)) = (browser, frame) else {
            return 1;
        };
        if !trusted_frame(frame) || browser.is_valid() == 0 {
            return 1;
        }
        let Some(main) = browser.main_frame() else {
            return 1;
        };
        let Some(mut owner) = frame.browser() else {
            return 1;
        };
        if browser.is_same(Some(&mut owner)) == 0
            || !trusted_frame(&main)
            || crate::strings::units(&main.identifier())
                != crate::strings::units(&frame.identifier())
        {
            return 1;
        }
        let Some(args) = message.argument_list() else {
            return 1;
        };
        if args.size() != 1 || args.get_type(0) != ValueType::STRING {
            return 1;
        }
        let Some(payload) = crate::strings::decode(&args.string(0), MAX_MESSAGE_BYTES) else {
            return 1;
        };
        if payload.is_empty() || payload.len() > MAX_MESSAGE_BYTES {
            return 1;
        }
        if let Ok(mut queue) = self.0.lock() {
            if queue.len() == MAX_PENDING_MESSAGES {
                queue.pop_front();
            }
            queue.push_back(payload);
        }
        1
    }

    pub fn next_message(&self) -> Option<String> {
        self.0.lock().ok()?.pop_front()
    }
}

fn trusted_frame(frame: &Frame) -> bool {
    frame.is_valid() != 0
        && frame.is_main() != 0
        && crate::strings::equals(&frame.url(), MANAGER_URL)
}

/// Pass this dictionary as create_browser[_sync]'s extra_info. Only HTTP(S)
/// backend configuration and the broker's session secret are transferred; neither
/// enables the privileged bridge.
pub fn renderer_extra_info(
    backend_url: &str,
    broker: Option<(&str, &str)>,
) -> Option<DictionaryValue> {
    let broker_url = broker.map(|(url, _)| url);
    if backend_url.len() > 8192
        || broker_url.is_some_and(|url| url.len() > 8192)
        || broker.is_some_and(|(_, secret)| !valid_secret(secret))
    {
        return None;
    }
    for value in std::iter::once(backend_url).chain(broker_url) {
        let url = Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
    }
    let info = dictionary_value_create()?;
    if info.set_string(Some(&BACKEND_KEY.into()), Some(&backend_url.into())) == 0 {
        return None;
    }
    if let Some((url, secret)) = broker
        && (info.set_string(Some(&BROKER_KEY.into()), Some(&url.into())) == 0
            || info.set_string(Some(&BROKER_SECRET_KEY.into()), Some(&secret.into())) == 0)
    {
        return None;
    }
    Some(info)
}

/// Return from App::render_process_handler in BOTH the browser application and
/// the App passed to execute_process before product CLI parsing.
pub fn renderer_handler() -> RenderProcessHandler {
    RendererBridge::new(
        Shared::new(RefCell::new(HashMap::new())),
        Shared::new(RefCell::new(HashMap::new())),
        Shared::new(RefCell::new(String::new())),
    )
}

/// Backend URL, and the broker URL with its session secret.
type RendererState = Shared<RefCell<HashMap<i32, (String, Option<(String, String)>)>>>;
type RendererSites = Shared<RefCell<HashMap<(i32, String), String>>>;

wrap_render_process_handler! {
    struct RendererBridge { state: RendererState, sites: RendererSites, identity: Shared<RefCell<String>> }
    impl RenderProcessHandler {
        fn on_web_kit_initialized(&self) {
            // Linux renderer children fork from CEF's zygote. Generate here,
            // after that fork, not while constructing the application's handler.
            let mut identity = [0u8; 32];
            getrandom::fill(&mut identity).expect("renderer identity entropy unavailable");
            *self.identity.borrow_mut() = identity.iter().map(|byte| format!("{byte:02x}")).collect();
        }
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let (Some(browser), Some(info)) = (browser, extra_info) else { return };
            let key: CefString = BACKEND_KEY.into();
            if info.get_type(Some(&key)) != ValueType::STRING { return; }
            let Some(backend) = crate::strings::decode(&info.string(Some(&key)), 8192) else { return };
            let string = |name: &str| {
                let key: CefString = name.into();
                (info.get_type(Some(&key)) == ValueType::STRING)
                    .then(|| crate::strings::decode(&info.string(Some(&key)), 8192))
                    .flatten()
            };
            let broker = string(BROKER_KEY).zip(string(BROKER_SECRET_KEY));
            if !valid_http_url(&backend)
                || broker.as_ref().is_some_and(|(url, secret)| !valid_http_url(url) || !valid_secret(secret))
            { return; }
            self.state.borrow_mut().insert(browser.identifier(), (backend, broker));
        }
        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                self.state.borrow_mut().remove(&browser.identifier());
                self.sites.borrow_mut().retain(|(id, _), _| *id != browser.identifier());
            }
        }
        fn on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            let (Some(browser), Some(frame), Some(message)) = (browser, frame, message) else { return 0 };
            let register = crate::strings::equals(&message.name(), crate::sites::REGISTER);
            let revoke = crate::strings::equals(&message.name(), crate::sites::REVOKE);
            let check = crate::strings::equals(&message.name(), crate::sites::CHECK);
            if !register && !revoke && !check { return 0; }
            if source_process != ProcessId::BROWSER || frame.is_main() != 0 { return 1; }
            let Some(id) = crate::strings::decode(&frame.identifier(), 256) else { return 1 };
            if revoke {
                if let Some(values) = crate::sites::values(message, 1) {
                    self.sites.borrow_mut().remove(&(browser.identifier(), values[0].clone()));
                }
                return 1;
            }
            let Some(values) = crate::sites::values(message, 3) else { return 1 };
            let Some(url) = crate::strings::decode(&frame.url(), 8192).and_then(|u| Url::parse(&u).ok()) else { return 1 };
            let Some(name) = crate::strings::decode(&frame.name(), 256) else { return 1 };
            if name.strip_prefix(crate::sites::NAME_PREFIX) != Some(values[0].as_str()) || values[2] != id || url.origin().ascii_serialization() != values[1] { return 1; }
            if check {
                let approved = self.sites.borrow().get(&(browser.identifier(), values[0].clone())) == Some(&values[1]);
                crate::sites::send(frame, crate::sites::ACTIVE, &[&values[0], &values[1], &id, self.identity.borrow().as_str(), if approved { "1" } else { "0" }]);
                return 1;
            }
            if url.path() != crate::sites::PREPARE { return 1; }
            self.sites.borrow_mut().insert((browser.identifier(), values[0].clone()), values[1].clone());
            crate::sites::send(frame, crate::sites::ACK, &[&values[0], &values[1], &id, self.identity.borrow().as_str()]);
            1
        }
        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else { return };
            // CEF requires non-null output slots even when their values are unused.
            let mut retval = None;
            let mut exception = None;
            if trusted_frame(frame) {
                if let Some((backend, Some((broker, secret)))) = self.state.borrow().get(&browser.identifier()) {
                    let script = backend_routing_script(backend, broker, secret);
                    context.eval(Some(&script.as_str().into()), None, 1, Some(&mut retval), Some(&mut exception));
                }
                let Some(global) = context.global() else { return };
                let mut handler = PostMessage::new(frame.clone());
                let Some(mut function) = v8_value_create_function(Some(&"__VOSULAY_NATIVE_POST__".into()), Some(&mut handler)) else { return };
                if global.set_value_bykey(Some(&"__VOSULAY_NATIVE_POST__".into()), Some(&mut function), V8Propertyattribute::default()) == 0 { return; }
                context.eval(Some(&MANAGER_SCRIPT.into()), Some(&MANAGER_URL.into()), 1, Some(&mut retval), Some(&mut exception));
            } else if let Some((backend, Some((broker, secret)))) = self.state.borrow().get(&browser.identifier()) {
                let name = crate::strings::decode(&frame.name(), 256).unwrap_or_default();
                let request = name.strip_prefix(crate::sites::NAME_PREFIX).unwrap_or("");
                let sites = self.sites.borrow();
                let site = sites.get(&(browser.identifier(), request.to_owned()));
                let url = crate::strings::decode(&frame.url(), 8192).and_then(|u| Url::parse(&u).ok());
                let approved = site.filter(|origin| url.as_ref().is_some_and(|url| url.origin().ascii_serialization() == **origin));
                let selected = approved.map_or(broker.as_str(), |origin| origin.as_str());
                let script = backend_routing_script(backend, selected, secret);
                context.eval(Some(&script.as_str().into()), None, 1, Some(&mut retval), Some(&mut exception));
            }
        }
    }
}

wrap_v8_handler! {
    struct PostMessage { frame: Frame }
    impl V8Handler {
        fn execute(&self, _name: Option<&CefString>, _object: Option<&mut V8Value>, arguments: Option<&[Option<V8Value>]>, _retval: Option<&mut Option<V8Value>>, exception: Option<&mut CefString>) -> i32 {
            let fail = || { if let Some(exception) = exception { *exception = "Unauthorized or invalid Vosulay bridge message".into(); } 1 };
            if !trusted_frame(&self.frame) { return fail(); }
            let Some(context) = v8_context_get_current_context() else { return fail() };
            let Some(current) = context.frame() else { return fail() };
            if !trusted_frame(&current) || crate::strings::units(&current.identifier()) != crate::strings::units(&self.frame.identifier()) { return fail(); }
            let Some(arguments) = arguments else { return fail() };
            if arguments.len() != 1 { return fail(); }
            let Some(value) = &arguments[0] else { return fail() };
            if value.is_string() == 0 { return fail(); }
            let payload = value.string_value();
            let units = crate::strings::units(&payload);
            if units.is_empty() || units.len() > MAX_MESSAGE_BYTES { return fail(); }
            let byte_len: usize = char::decode_utf16(units.iter().copied())
                .map(|ch| ch.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8()).sum();
            if byte_len > MAX_MESSAGE_BYTES { return fail(); }
            let Some(mut message) = process_message_create(Some(&MESSAGE_NAME.into())) else { return fail() };
            let Some(args) = message.argument_list() else { return fail() };
            let raw: Option<&cef::sys::_cef_string_utf16_t> = (&payload).into();
            let Some(raw) = raw else { return fail() };
            // Borrow only while payload lives. set_string copies into the IPC
            // message, avoiding UTF-16 -> UTF-8 -> UTF-16 round trips.
            let text = CefString::from(raw as *const cef::sys::_cef_string_utf16_t);
            if args.set_string(0, Some(&text)) == 0 { return fail(); }
            self.frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
            1
        }
    }
}

const MANAGER_SCRIPT: &str = r#"(() => {
  const nativePost = window.__VOSULAY_NATIVE_POST__;
  delete window.__VOSULAY_NATIVE_POST__;
  const documentId = typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`;
  const post = payload => {
    let encoded = typeof payload === 'string' ? payload : JSON.stringify(payload);
    if (typeof encoded !== 'string' || encoded.length > 262144) throw new Error('Invalid bridge payload');
    try {
      const message = JSON.parse(encoded);
      if (message && typeof message === 'object' && !Array.isArray(message)) {
        message.__vosulay_document_id = documentId;
        encoded = JSON.stringify(message);
      }
    } catch (_) {}
    nativePost(encoded);
  };
  window.__VOSULAY_POST_MESSAGE__ = post;
  Object.defineProperty(window, '__VOSULAY_NATIVE_SITES__', { value: true });
  window.ipc = { postMessage: post };
  window.dispatchEvent(new Event('vosulay-bridge-ready'));
})();"#;

fn valid_http_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn valid_secret(secret: &str) -> bool {
    secret.len() == 64
        && secret
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn backend_routing_script(backend: &str, broker: &str, secret: &str) -> String {
    let args = serde_json::to_string(&serde_json::json!([backend, broker, null, secret]))
        .expect("string serialization cannot fail");
    format!(
        "({})(...{})",
        include_str!("../../../assets/backend.js"),
        args
    )
}
