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
/// backend configuration is transferred; it never enables the privileged bridge.
pub fn renderer_extra_info(backend_url: &str, broker_url: Option<&str>) -> Option<DictionaryValue> {
    if backend_url.len() > 8192 || broker_url.is_some_and(|url| url.len() > 8192) {
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
    if let Some(broker) = broker_url
        && info.set_string(Some(&BROKER_KEY.into()), Some(&broker.into())) == 0
    {
        return None;
    }
    Some(info)
}

/// Return from App::render_process_handler in BOTH the browser application and
/// the App passed to execute_process before product CLI parsing.
pub fn renderer_handler() -> RenderProcessHandler {
    RendererBridge::new(Shared::new(RefCell::new(HashMap::new())))
}

type RendererState = Shared<RefCell<HashMap<i32, (String, Option<String>)>>>;

wrap_render_process_handler! {
    struct RendererBridge { state: RendererState }
    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let (Some(browser), Some(info)) = (browser, extra_info) else { return };
            let key: CefString = BACKEND_KEY.into();
            if info.get_type(Some(&key)) != ValueType::STRING { return; }
            let Some(backend) = crate::strings::decode(&info.string(Some(&key)), 8192) else { return };
            let broker_key: CefString = BROKER_KEY.into();
            let broker = if info.get_type(Some(&broker_key)) == ValueType::STRING {
                crate::strings::decode(&info.string(Some(&broker_key)), 8192)
            } else { None };
            if !valid_http_url(&backend) || broker.as_deref().is_some_and(|url| !valid_http_url(url)) { return; }
            self.state.borrow_mut().insert(browser.identifier(), (backend, broker));
        }
        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser { self.state.borrow_mut().remove(&browser.identifier()); }
        }
        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else { return };
            // CEF requires non-null output slots even when their values are unused.
            let mut retval = None;
            let mut exception = None;
            if trusted_frame(frame) {
                if let Some((backend, Some(broker))) = self.state.borrow().get(&browser.identifier()) {
                    let script = backend_routing_script(backend, broker);
                    context.eval(Some(&script.as_str().into()), None, 1, Some(&mut retval), Some(&mut exception));
                }
                let Some(global) = context.global() else { return };
                let mut handler = PostMessage::new(frame.clone());
                let Some(mut function) = v8_value_create_function(Some(&"__VOSULAY_NATIVE_POST__".into()), Some(&mut handler)) else { return };
                if global.set_value_bykey(Some(&"__VOSULAY_NATIVE_POST__".into()), Some(&mut function), V8Propertyattribute::default()) == 0 { return; }
                context.eval(Some(&MANAGER_SCRIPT.into()), Some(&MANAGER_URL.into()), 1, Some(&mut retval), Some(&mut exception));
            } else if let Some((backend, Some(broker))) = self.state.borrow().get(&browser.identifier()) {
                let script = backend_routing_script(backend, broker);
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

fn backend_routing_script(backend: &str, broker: &str) -> String {
    let args = serde_json::to_string(&[backend, broker]).expect("string serialization cannot fail");
    format!(
        "({})(...{})",
        include_str!("../../../assets/backend.js"),
        args
    )
}
