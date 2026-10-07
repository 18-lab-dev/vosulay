use cef::*;
use std::sync::{Arc, Mutex};
use url::Url;

pub const SCHEME: &str = "vosulay";
pub const OVERLAY_SCHEME: &str = "vosulay-overlay";
pub const MANAGER_URL: &str = "vosulay://manager/index.html";
pub const MAX_RESOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_URL_BYTES: usize = 8192;

/// Paths remain URL-encoded. Local routes MUST be resolved by the product's
/// canonical OverlayAuthorization service, never joined directly onto a root.
#[derive(Debug)]
pub enum ResourceRoute<'a> {
    Manager { path: &'a str },
    Local { capability: &'a str, path: &'a str },
    Smoke { path: &'a str },
}

/// Called on CEF's IO thread. Return None for unknown/unauthorized resources.
/// Manager must serve embedded application assets only, never user HTML.
/// Local must reauthorize the capability and canonical file on every request.
/// Implementations must bound file reads by MAX_RESOURCE_BYTES before allocation.
pub trait ResourceService: Send + Sync {
    fn load(&self, route: ResourceRoute<'_>) -> Option<Resource>;
}

pub struct Resource {
    pub bytes: Arc<[u8]>,
    pub mime_type: String,
    pub status: u16,
}

/// Delegate App::on_register_custom_schemes in every process to this function.
pub fn register_scheme(registrar: Option<&mut SchemeRegistrar>) -> bool {
    let Some(registrar) = registrar else {
        return false;
    };
    let flags = SchemeOptions::STANDARD.get_raw()
        | SchemeOptions::SECURE.get_raw()
        | SchemeOptions::CORS_ENABLED.get_raw()
        | SchemeOptions::FETCH_ENABLED.get_raw();
    #[cfg(not(windows))]
    let flags = flags as i32;
    let manager = registrar.add_custom_scheme(Some(&SCHEME.into()), flags) != 0;
    let overlay = registrar.add_custom_scheme(Some(&OVERLAY_SCHEME.into()), flags) != 0;
    manager && overlay
}

/// Call on the browser UI thread after CEF initialization (or context_initialized).
pub fn install_scheme(service: Arc<dyn ResourceService>) -> bool {
    let mut factory = LocalFactory::new(service);
    let manager =
        register_scheme_handler_factory(Some(&SCHEME.into()), None, Some(&mut factory)) != 0;
    let overlay =
        register_scheme_handler_factory(Some(&OVERLAY_SCHEME.into()), None, Some(&mut factory))
            != 0;
    manager && overlay
}

fn route(url: &Url) -> Option<ResourceRoute<'_>> {
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path();
    match url.scheme() {
        SCHEME => match url.host_str()? {
            "manager" => Some(ResourceRoute::Manager { path }),
            "smoke" => Some(ResourceRoute::Smoke { path }),
            _ => None,
        },
        OVERLAY_SCHEME => {
            let capability = url.host_str()?;
            if capability.len() != 32
                || !capability
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            Some(ResourceRoute::Local {
                capability,
                path: path.strip_prefix('/')?,
            })
        }
        _ => None,
    }
}

wrap_scheme_handler_factory! {
    struct LocalFactory { service: Arc<dyn ResourceService> }
    impl SchemeHandlerFactory {
        fn create(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, scheme_name: Option<&CefString>, request: Option<&mut Request>) -> Option<ResourceHandler> {
            if ![SCHEME, OVERLAY_SCHEME].iter().any(|scheme| scheme_name.as_ref().is_some_and(|name| name.to_string() == *scheme)) { return None; }
            let request = request?;
            let head = crate::strings::equals(&request.method(), "HEAD");
            if !head && !crate::strings::equals(&request.method(), "GET") { return None; }
            let raw_url = crate::strings::decode(&request.url(), MAX_URL_BYTES)?;
            let url = Url::parse(&raw_url).ok()?;
            let resource = self.service.load(route(&url)?)?;
            if resource.bytes.len() > MAX_RESOURCE_BYTES || !(200..=599).contains(&resource.status)
                || resource.mime_type.is_empty() || resource.mime_type.len() > 128
                || !resource.mime_type.bytes().all(|b| b.is_ascii_graphic()) {
                return None;
            }
            Some(LocalResource::new(Arc::new(resource), Arc::new(Mutex::new(0usize)), head))
        }
    }
}

wrap_resource_handler! {
    struct LocalResource { resource: Arc<Resource>, cursor: Arc<Mutex<usize>>, head: bool }
    impl ResourceHandler {
        fn open(&self, _request: Option<&mut Request>, handle_request: Option<&mut i32>, _callback: Option<&mut Callback>) -> i32 {
            let Some(handle_request) = handle_request else { return 0 };
            *handle_request = 1;
            1
        }
        fn response_headers(&self, response: Option<&mut Response>, response_length: Option<&mut i64>, _redirect_url: Option<&mut CefString>) {
            if let Some(length) = response_length { *length = if self.head { 0 } else { self.resource.bytes.len() as i64 }; }
            if let Some(response) = response {
                response.set_status(self.resource.status as i32);
                response.set_mime_type(Some(&self.resource.mime_type.as_str().into()));
                response.set_header_by_name(Some(&"X-Content-Type-Options".into()), Some(&"nosniff".into()), 1);
                response.set_header_by_name(Some(&"Cache-Control".into()), Some(&"no-store".into()), 1);
                // Do not grant cross-origin access or bypass the document's CSP.
            }
        }
        fn skip(&self, bytes_to_skip: i64, bytes_skipped: Option<&mut i64>, _callback: Option<&mut ResourceSkipCallback>) -> i32 {
            let Some(skipped) = bytes_skipped else { return 0 };
            *skipped = 0;
            if self.head || bytes_to_skip <= 0 { return 0; }
            let Ok(mut cursor) = self.cursor.lock() else { return 0 };
            let count = (bytes_to_skip as u64).min((self.resource.bytes.len() - *cursor) as u64) as usize;
            *cursor += count;
            *skipped = count as i64;
            i32::from(count != 0)
        }
        fn read(&self, data_out: *mut u8, bytes_to_read: i32, bytes_read: Option<&mut i32>, _callback: Option<&mut ResourceReadCallback>) -> i32 {
            let Some(bytes_read) = bytes_read else { return 0 };
            *bytes_read = 0;
            if self.head || data_out.is_null() || bytes_to_read <= 0 { return 0; }
            let Ok(mut cursor) = self.cursor.lock() else { return 0 };
            let count = (bytes_to_read as usize).min(self.resource.bytes.len() - *cursor);
            if count == 0 { return 0; }
            // CEF owns a writable bytes_to_read-byte output buffer for this callback.
            unsafe { std::ptr::copy_nonoverlapping(self.resource.bytes.as_ptr().add(*cursor), data_out, count); }
            *cursor += count;
            *bytes_read = count as i32;
            1
        }
        fn cancel(&self) {
            if let Ok(mut cursor) = self.cursor.lock() { *cursor = self.resource.bytes.len(); }
        }
    }
}
