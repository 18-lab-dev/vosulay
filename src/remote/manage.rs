//! The manage page: vosulay's full editor at `http://manage.localhost:<port>/overlay`,
//! for this computer only and only with Settings → Advanced → Allow editing on this
//! computer. Requests go through the app's editor worker, so the in-game editor and
//! the saved file stay consistent. The manage origin serves nothing but the editor:
//! counters on the page load from the listener's own address, a different origin,
//! so counter scripts cannot reach the editing routes.

use std::net::SocketAddr;

use anyhow::Result;
use serde_json::{Value, json};
use url::Url;

use super::{Backend, HttpError, Request, Response, normalize_ip, request_authority};
use crate::config::AppConfig;

pub(super) const HOST: &str = "manage.localhost";
pub(super) const MAX_BODY: usize = 1024 * 1024;
// The editor must never render inside another page: a counter or website framing it
// could trick clicks into same-origin edits that the API's Origin checks would allow.
// Host windows and OBS sources load it top-level, so no embedding is needed.
const PAGE_POLICY: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; frame-src 'self' http: https:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const DISABLED: &str =
    "The editor is limited to this computer and must be enabled in Settings → Advanced\n";

/// The editing authority behind the manage page: the app's editor worker.
pub trait ManageEditor: Send + Sync {
    /// `{"config": <AppConfig>, "revision": n}`.
    fn state(&self) -> Result<Value>;
    /// Handles one manager message (saves, native file dialogs, vosu overlay settings)
    /// and returns the manager envelopes it produced.
    fn message(&self, message: Value) -> Result<Value>;
    /// The catalog envelope (`counters`, or `error`).
    fn counters(&self) -> Value;
}

/// Whether `host` (a request's Host header) names the manage page.
pub(super) fn is_manage_host(host: Option<&str>, local_address: SocketAddr) -> bool {
    host.and_then(|host| request_authority(host, local_address))
        .is_some_and(|authority| authority.host_str() == Some(HOST))
}

/// Editing is offered only when enabled, available, and the client is this machine.
pub(super) fn enabled(config: &AppConfig, backend: &Backend, peer: SocketAddr) -> bool {
    config.settings.allow_local_profile_editing
        && !backend.tool_mode
        && backend.editor.is_some()
        && normalize_ip(peer.ip()).is_loopback()
}

/// Routes a request that arrived for the manage host. Default deny: nothing is served
/// unless editing is enabled for this peer, and only the page and its API exist here.
pub(super) fn respond(
    request: &Request,
    route: &str,
    config: &AppConfig,
    backend: &Backend,
    peer: SocketAddr,
    local_address: SocketAddr,
) -> std::result::Result<Response, HttpError> {
    if !enabled(config, backend, peer) {
        return Err(HttpError::new(403, "Forbidden", DISABLED));
    }
    if route == "overlay" {
        // Refused before rendering too, for clients that ignore frame-ancestors.
        if request
            .header("sec-fetch-dest")
            .is_some_and(|destination| destination != "document")
        {
            return Err(HttpError::new(
                403,
                "Forbidden",
                "The editor cannot be embedded; open it directly\n",
            ));
        }
        return page(&view_url(local_address)?, &backend.upstream);
    }
    match route.strip_prefix("__vosulay/manage/") {
        Some(action) => api(request, backend, action, local_address),
        None => Err(HttpError::new(404, "Not Found", "Route not found\n")),
    }
}

/// Where the manage page's counters load: this listener's own address.
fn view_url(local_address: SocketAddr) -> std::result::Result<Url, HttpError> {
    let address = SocketAddr::new(normalize_ip(local_address.ip()), local_address.port());
    Url::parse(&format!("http://{address}/"))
        .map_err(|_| HttpError::new(500, "Internal Server Error", "Invalid listener address\n"))
}

pub(super) fn page(view: &Url, upstream: &Url) -> std::result::Result<Response, HttpError> {
    let routing = json!([upstream.as_str(), view.as_str(), true]);
    let prelude = format!(
        "({})(...{});({})({});",
        include_str!("../../assets/backend.js"),
        crate::ui::script_json(&routing),
        include_str!("../../assets/manage-bridge.js"),
        crate::ui::script_json(&json!(view.as_str()))
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
        ("X-Frame-Options".into(), "DENY".to_owned()),
    ]);
    Ok(response)
}

/// Public routing identity, never a broker credential. Revisions can repeat after restart.
pub(super) fn state(editor: &dyn ManageEditor, view: &Url, upstream: &Url) -> Result<Value> {
    static GENERATION: std::sync::LazyLock<Result<String>> = std::sync::LazyLock::new(|| {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        let mut identity = String::with_capacity(32);
        for byte in bytes {
            std::fmt::Write::write_fmt(&mut identity, format_args!("{byte:02x}"))?;
        }
        Ok(identity)
    });
    let generation = GENERATION
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let mut state = editor.state()?;
    state
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid manager state"))?
        .insert(
            "routing".into(),
            json!({"generation": generation, "backend": upstream, "view": view}),
        );
    Ok(state)
}

fn api(
    request: &Request,
    backend: &Backend,
    action: &str,
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
    let failed =
        |error: anyhow::Error| HttpError::new(500, "Internal Server Error", format!("{error:#}\n"));
    let value = match (action, post) {
        ("state", false) => {
            state(editor, &view_url(local_address)?, &backend.upstream).map_err(failed)?
        }
        ("counters", false) => editor.counters(),
        ("message", true) => {
            let message: Value = serde_json::from_slice(&request.body)
                .map_err(|_| HttpError::new(400, "Bad Request", "Request body is not JSON\n"))?;
            editor.message(message).map_err(failed)?
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

/// Browsers mark same-origin requests; other pages on this machine, including the
/// counters on the manage page itself, must not drive the editor. POSTs must also
/// carry the manage origin.
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

#[cfg(test)]
mod tests {
    use super::super::RemoteServer;
    use super::*;
    use parking_lot::Mutex;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct FakeEditor {
        messages: Mutex<Vec<Value>>,
    }

    impl ManageEditor for FakeEditor {
        fn state(&self) -> Result<Value> {
            Ok(json!({"config": {"profiles": []}, "revision": 7}))
        }
        fn message(&self, message: Value) -> Result<Value> {
            self.messages.lock().push(message);
            Ok(json!([{"type": "saved", "revision": 8}]))
        }
        fn counters(&self) -> Value {
            json!({"type": "counters", "counters": [], "vosu": null})
        }
    }

    fn send(address: SocketAddr, host: &str, head: &str, body: &str) -> String {
        let mut client = TcpStream::connect(address).expect("connect");
        write!(
            client,
            "{head}\r\nHost: {host}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("write");
        client.shutdown(Shutdown::Write).expect("shutdown");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        response
    }

    #[test]
    fn manage_host_is_default_deny_same_origin_and_serves_only_the_editor() {
        let enabled = Arc::new(AtomicBool::new(false));
        let editor = Arc::new(FakeEditor::default());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let setting = enabled.clone();
        let server = RemoteServer::start_with_listeners(
            vec![listener],
            Url::parse("http://127.0.0.1:9/").unwrap(),
            Arc::new(move || {
                let mut config = AppConfig::default();
                config.settings.allow_local_profile_editing = setting.load(Ordering::Acquire);
                Ok(config)
            }),
            Some(editor.clone()),
            None,
        )
        .expect("server");
        let manage = format!("{HOST}:{}", address.port());
        let view = address.to_string();
        let same_origin = "Sec-Fetch-Site: same-origin";
        let state = format!("GET /__vosulay/manage/state HTTP/1.1\r\n{same_origin}");
        let post = |headers: &str| {
            send(
                address,
                &manage,
                &format!("POST /__vosulay/manage/message HTTP/1.1\r\n{headers}"),
                r#"{"type":"save-config"}"#,
            )
        };
        let json_post =
            format!("{same_origin}\r\nOrigin: http://{manage}\r\nContent-Type: application/json");

        // Off by default: no editor and no editing API anywhere.
        assert!(send(address, &manage, "GET /overlay HTTP/1.1", "").starts_with("HTTP/1.1 403 "));
        assert!(send(address, &manage, &state, "").starts_with("HTTP/1.1 403 "));
        assert!(post(&json_post).starts_with("HTTP/1.1 403 "));

        enabled.store(true, Ordering::Release);
        let page = send(address, &manage, "GET /overlay HTTP/1.1", "");
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.contains("/__vosulay/manage/"));
        // Counters on the editor load from the listener address, not the manage origin.
        assert!(page.contains(&format!("http://{view}/")));
        assert!(page.contains("frame-ancestors 'none'"));
        // A counter or website framing the editor (cross-site iframe navigation) is refused.
        let framed = "GET /overlay HTTP/1.1\r\nSec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: iframe";
        assert!(send(address, &manage, framed, "").starts_with("HTTP/1.1 403 "));
        let opened = "GET /overlay HTTP/1.1\r\nSec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document";
        assert!(send(address, &manage, opened, "").starts_with("HTTP/1.1 200 OK\r\n"));
        let reply = send(address, &manage, &state, "");
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(reply.contains(r#""revision":7"#));

        // The plain address stays view-only, and the manage origin serves no counters.
        let static_page = send(address, &view, "GET /overlay HTTP/1.1", "");
        assert!(static_page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(!static_page.contains("/__vosulay/manage/"));
        assert!(send(address, &view, &state, "").starts_with("HTTP/1.1 404 "));
        assert!(send(address, &manage, "GET /accuracy/ HTTP/1.1", "").starts_with("HTTP/1.1 404 "));

        // Other sites (and the page's own counters), and POSTs without the manage Origin, are refused.
        let cross = "GET /__vosulay/manage/state HTTP/1.1\r\nSec-Fetch-Site: same-site";
        assert!(send(address, &manage, cross, "").starts_with("HTTP/1.1 403 "));
        let counter_origin =
            format!("{same_origin}\r\nOrigin: http://{view}\r\nContent-Type: application/json");
        assert!(post(&counter_origin).starts_with("HTTP/1.1 403 "));
        assert!(
            post(&format!("{same_origin}\r\nContent-Type: application/json"))
                .starts_with("HTTP/1.1 403 ")
        );
        assert!(
            post(&format!(
                "{same_origin}\r\nOrigin: http://{manage}\r\nContent-Type: text/plain"
            ))
            .starts_with("HTTP/1.1 415 ")
        );
        assert!(editor.messages.lock().is_empty());

        let saved = post(&json_post);
        assert!(saved.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(saved.contains(r#""revision":8"#));
        assert_eq!(
            editor.messages.lock().as_slice(),
            [json!({"type": "save-config"})]
        );
        drop(server);
    }

    #[test]
    fn editing_is_offered_only_to_this_machine() {
        let mut config = AppConfig::default();
        config.settings.allow_local_profile_editing = true;
        let mut backend = Backend {
            upstream: Url::parse("http://127.0.0.1:24050/").unwrap(),
            broker: crate::backend::Broker::new(
                Url::parse("http://127.0.0.1:24050/").unwrap(),
                None,
            )
            .unwrap(),
            secret: "0".repeat(64),
            http: ureq::Agent::new_with_defaults(),
            editor: Some(Arc::new(FakeEditor::default())),
            tool_mode: false,
            preview_url: None,
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
        backend.tool_mode = true;
        assert!(!enabled(
            &config,
            &backend,
            "127.0.0.1:40000".parse().unwrap()
        ));
        config.settings.allow_local_profile_editing = false;
        assert!(!enabled(
            &config,
            &backend,
            "127.0.0.1:40000".parse().unwrap()
        ));
    }
}
