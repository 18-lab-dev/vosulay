//! Local profile editing for remote profile pages (Settings → Advanced → Allow
//! editing on this computer). Pages opened from vosulay's own machine get the
//! manager editor scoped to the profile they show; edits go through the app's
//! editor worker so the in-game editor and the saved file stay consistent.

use std::net::SocketAddr;

use anyhow::Result;
use serde_json::{Value, json};
use url::Url;

use super::{
    Backend, HttpError, Request, Response, backend_script, placement_url, request_authority,
};
use crate::config::{AppConfig, Profile};

pub(super) const MAX_BODY: usize = 1024 * 1024;
const PAGE_POLICY: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; frame-src 'self' http: https:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors *";

/// The editing authority behind remote profile pages: the app's editor worker.
pub trait ProfileEditor: Send + Sync {
    /// `{"config": <AppConfig with only this profile>, "revision": n}`.
    fn state(&self, profile_id: &str) -> Result<Value>;
    /// Applies `{"save_id", "base_revision", "profile"}` to this profile's
    /// placements; returns the manager's `saved` or `save-error` envelope.
    fn save(&self, profile_id: &str, request: Value) -> Result<Value>;
    /// Opens the backend's settings page for a counter on this machine.
    fn open_settings(&self, counter_id: &str) -> Result<()>;
}

/// Editing is offered only when enabled, available, and the client is this machine.
pub(super) fn enabled(config: &AppConfig, backend: &Backend, peer: SocketAddr) -> bool {
    config.settings.allow_local_profile_editing
        && backend.editor.is_some()
        && super::normalize_ip(peer.ip()).is_loopback()
}

pub(super) fn page(
    profile: &Profile,
    serving_url: &Url,
    upstream: &Url,
) -> std::result::Result<Response, HttpError> {
    let arguments = json!([profile.id, local_urls(profile, serving_url, upstream)]);
    let prelude = format!(
        "{}({})(...{});",
        backend_script(upstream, serving_url),
        include_str!("../../assets/remote-editor.js"),
        crate::ui::script_json(&arguments)
    );
    debug_assert!(!prelude.contains("</"));
    let mut response = Response::bytes(
        200,
        "OK",
        "text/html; charset=utf-8",
        crate::ui::manager_document_with(&prelude).into_bytes(),
    );
    response.headers.extend([
        ("Cache-Control".into(), "no-store".to_owned()),
        ("Referrer-Policy".into(), "no-referrer".to_owned()),
        ("X-Content-Type-Options".into(), "nosniff".to_owned()),
        ("Content-Security-Policy".into(), PAGE_POLICY.to_owned()),
    ]);
    Ok(response)
}

/// `vosulay-overlay:` placements as this server's `/local/` URLs.
fn local_urls(profile: &Profile, serving_url: &Url, upstream: &Url) -> Value {
    let mut urls = serde_json::Map::new();
    for item in &profile.overlays {
        if let Some(url) = item
            .url
            .as_ref()
            .filter(|url| url.scheme() == "vosulay-overlay")
            && let Ok(served) = placement_url(item, serving_url, upstream)
        {
            urls.insert(url.to_string(), Value::String(served));
        }
    }
    Value::Object(urls)
}

pub(super) fn api(
    request: &Request,
    backend: &Backend,
    action: &str,
    serving_url: &Url,
    local_address: SocketAddr,
) -> std::result::Result<Response, HttpError> {
    let forbidden = || HttpError::new(403, "Forbidden", "Request is not authorized\n");
    let post = request.method == "POST";
    if !same_origin(request, local_address, post) {
        return Err(forbidden());
    }
    if post
        && !request
            .header("content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(HttpError::new(
            415,
            "Unsupported Media Type",
            "Use application/json\n",
        ));
    }
    let editor = backend.editor.as_deref().ok_or_else(forbidden)?;
    let profile_id = profile_id(&request.target)?;
    let failed =
        |error: anyhow::Error| HttpError::new(500, "Internal Server Error", format!("{error:#}\n"));
    let value = match (action, post) {
        ("state", false) => {
            let mut state = editor.state(&profile_id).map_err(failed)?;
            let profile: Option<Profile> = state
                .pointer("/config/profiles/0")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok());
            if let Some(profile) = profile {
                state["local_urls"] = local_urls(&profile, serving_url, &backend.upstream);
            }
            state
        }
        ("counters", false) => match crate::api::fetch_installed_counters(&backend.upstream) {
            Ok(counters) => json!({"type": "counters", "counters": counters}),
            Err(error) => json!({"type": "error", "message": format!("{error:#}")}),
        },
        ("save", true) => editor
            .save(&profile_id, read_json(request)?)
            .map_err(failed)?,
        ("open-settings", true) => {
            let body = read_json(request)?;
            let counter = body
                .get("counter_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| HttpError::new(400, "Bad Request", "Missing counter_id\n"))?;
            editor.open_settings(counter).map_err(failed)?;
            Value::Null
        }
        _ => return Err(HttpError::new(404, "Not Found", "Route not found\n")),
    };
    let mut response = Response::bytes(
        200,
        "OK",
        "application/json",
        value.to_string().into_bytes(),
    );
    response
        .headers
        .push(("Cache-Control".into(), "no-store".to_owned()));
    Ok(response)
}

/// Browsers mark same-origin requests; a cross-site page on this machine must
/// not drive the editor. POSTs must also carry this server's own Origin.
fn same_origin(request: &Request, local_address: SocketAddr, post: bool) -> bool {
    let Some(authority) = request
        .host
        .as_deref()
        .and_then(|host| request_authority(host, local_address))
    else {
        return false;
    };
    let origin = request
        .origin
        .as_deref()
        .map(|origin| Url::parse(origin).is_ok_and(|url| url.origin() == authority.origin()));
    match (request.fetch_site.as_deref(), origin) {
        (_, Some(false)) => false,
        (Some(site), _) if site != "same-origin" => false,
        (_, Some(true)) => true,
        (Some(_), None) => !post,
        (None, None) => false,
    }
}

fn profile_id(target: &str) -> std::result::Result<String, HttpError> {
    let query = target.split_once('?').map_or("", |(_, query)| query);
    let mut id = None;
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if name != "id" || id.is_some() {
            return Err(HttpError::new(
                400,
                "Bad Request",
                "Expected one id parameter\n",
            ));
        }
        id = Some(value.into_owned());
    }
    id.filter(|id| !id.is_empty())
        .ok_or_else(|| HttpError::new(400, "Bad Request", "Missing profile id\n"))
}

fn read_json(request: &Request) -> std::result::Result<Value, HttpError> {
    serde_json::from_slice(&request.body)
        .map_err(|_| HttpError::new(400, "Bad Request", "Request body is not JSON\n"))
}

#[cfg(test)]
mod tests {
    use super::super::RemoteServer;
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeEditor {
        saves: Mutex<Vec<(String, Value)>>,
    }

    impl ProfileEditor for FakeEditor {
        fn state(&self, profile_id: &str) -> Result<Value> {
            Ok(
                json!({"config": {"profiles": [{"id": profile_id, "name": "P", "overlays": []}]}, "revision": 7}),
            )
        }
        fn save(&self, profile_id: &str, request: Value) -> Result<Value> {
            self.saves
                .lock()
                .unwrap()
                .push((profile_id.to_owned(), request));
            Ok(json!({"type": "saved", "revision": 8}))
        }
        fn open_settings(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn send(address: SocketAddr, head: &str, body: &str) -> String {
        let mut client = TcpStream::connect(address).expect("connect");
        write!(
            client,
            "{head}\r\nHost: {address}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("write");
        client.shutdown(Shutdown::Write).expect("shutdown");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        response
    }

    #[test]
    fn local_editing_needs_the_setting_and_same_origin_json_requests() {
        let enabled = Arc::new(AtomicBool::new(false));
        let editor = Arc::new(FakeEditor::default());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let setting = enabled.clone();
        let server = RemoteServer::start_with_listeners(
            vec![listener],
            Url::parse("http://127.0.0.1:24050/").unwrap(),
            Arc::new(move || {
                let mut config = AppConfig::default();
                config.settings.allow_local_profile_editing = setting.load(Ordering::Acquire);
                Ok(config)
            }),
            Some(editor.clone()),
        )
        .expect("server");
        let same_origin = "Sec-Fetch-Site: same-origin";
        let state = format!("GET /__vosulay/edit/state?id=default HTTP/1.1\r\n{same_origin}");
        let save = |headers: &str| {
            send(
                address,
                &format!("POST /__vosulay/edit/save?id=default HTTP/1.1\r\n{headers}"),
                r#"{"save_id":"s","base_revision":7,"profile":{"id":"default"}}"#,
            )
        };
        let json_post =
            format!("{same_origin}\r\nOrigin: http://{address}\r\nContent-Type: application/json");

        // Off by default: view-only page and no editing API.
        assert!(!send(address, "GET /overlay HTTP/1.1", "").contains("/__vosulay/edit/"));
        assert!(send(address, &state, "").starts_with("HTTP/1.1 403 "));
        assert!(save(&json_post).starts_with("HTTP/1.1 403 "));

        enabled.store(true, Ordering::Release);
        let page = send(address, "GET /overlay HTTP/1.1", "");
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.contains("/__vosulay/edit/"));
        let reply = send(address, &state, "");
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(reply.contains(r#""revision":7"#));

        // Other sites on this machine, and POSTs without this server's Origin, are refused.
        let cross = "GET /__vosulay/edit/state?id=default HTTP/1.1\r\nSec-Fetch-Site: same-site";
        assert!(send(address, cross, "").starts_with("HTTP/1.1 403 "));
        assert!(
            save(&format!("{same_origin}\r\nContent-Type: application/json"))
                .starts_with("HTTP/1.1 403 ")
        );
        assert!(
            save(&format!(
                "{same_origin}\r\nOrigin: http://{address}\r\nContent-Type: text/plain"
            ))
            .starts_with("HTTP/1.1 415 ")
        );
        assert!(editor.saves.lock().unwrap().is_empty());

        assert!(save(&json_post).starts_with("HTTP/1.1 200 OK\r\n"));
        let saves = editor.saves.lock().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].0, "default");
        assert_eq!(saves[0].1["base_revision"], 7);
        drop(saves);
        drop(server);
    }

    #[test]
    fn editing_is_offered_only_to_this_machine() {
        let mut config = AppConfig::default();
        config.settings.allow_local_profile_editing = true;
        let backend = Backend {
            upstream: Url::parse("http://127.0.0.1:24050/").unwrap(),
            broker: crate::backend::Broker::new(Url::parse("http://127.0.0.1:24050/").unwrap())
                .unwrap(),
            http: ureq::Agent::new_with_defaults(),
            editor: Some(Arc::new(FakeEditor::default())),
        };
        assert!(enabled(
            &config,
            &backend,
            "127.0.0.1:40000".parse().unwrap()
        ));
        assert!(enabled(
            &config,
            &backend,
            "[::ffff:127.0.0.1]:40000".parse().unwrap()
        ));
        assert!(!enabled(
            &config,
            &backend,
            "192.168.1.20:40000".parse().unwrap()
        ));
        config.settings.allow_local_profile_editing = false;
        assert!(!enabled(
            &config,
            &backend,
            "127.0.0.1:40000".parse().unwrap()
        ));
    }
}
