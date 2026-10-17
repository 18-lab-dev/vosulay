//! The editor's native overlay settings: vosu's settings HTTP API, read openly and written with
//! vosu's per-start broker token (`broker.json`). Requests run in vosulay's host process, so page
//! JavaScript never sees the token; it is only ever sent to a loopback vosu (see
//! `BrokerCredential`).
use std::{fmt, sync::Mutex, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use url::Url;

use super::{fetch_vosu_registry, get_json, parse_vosu_registry};
use crate::{backend::BrokerCredential, config::valid_vosu_profile};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_REPLY_BYTES: usize = 64 * 1024;
const REMOTE: &str = "vosu is not on this computer, so vosulay cannot save its settings";
const REJECTED: &str = "vosu did not accept vosulay's token, even after reading it again";

/// A write vosulay cannot authorize: the editor shows the reason and stays read-only.
#[derive(Debug)]
struct ReadOnly(String);

impl fmt::Display for ReadOnly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReadOnly {}

pub(crate) struct VosuClient {
    backend: Url,
    credential: Option<BrokerCredential>,
    // The token vosu refused last; until `broker.json` holds another one, writes stay off.
    rejected: Mutex<Option<String>>,
    timeout: Duration,
}

impl VosuClient {
    pub(crate) fn new(backend: Url, timeout: Duration) -> Self {
        let credential = BrokerCredential::for_backend(&backend);
        Self::with_credential(backend, credential, timeout)
    }

    pub(crate) fn with_credential(
        backend: Url,
        credential: Option<BrokerCredential>,
        timeout: Duration,
    ) -> Self {
        Self {
            backend,
            credential,
            rejected: Mutex::new(None),
            timeout,
        }
    }

    /// Whether vosulay holds a token vosu has not refused, or why not. Each call reads
    /// `broker.json` again when there is no usable token, so the editor's retry picks up a vosu
    /// that started, restarted or was fixed since.
    pub(crate) fn write_access(&self) -> Result<(), String> {
        let credential = self.credential.as_ref().ok_or(REMOTE)?;
        let token = credential.status()?;
        if self.rejected().as_deref() != Some(token.as_str()) {
            return Ok(());
        }
        match credential.reread() {
            Some(fresh) if fresh != token => Ok(()),
            Some(_) => Err(REJECTED.to_owned()),
            None => credential.status().map(|_| ()),
        }
    }

    /// The catalog envelope, with `vosu.writable` and `vosu.reason`, within `timeout`.
    pub(crate) fn counters_envelope(&self, timeout: Duration) -> Value {
        super::counters_envelope(&self.backend, timeout, self.write_access())
    }

    /// Handles one `vosu-settings-load`, `vosu-settings-save` or `vosu-profile-action` manager
    /// message and returns its reply envelope; failures become `vosu-settings-error`, with
    /// `read_only` when vosulay cannot authorize writes.
    pub(crate) fn handle(&self, message: &Value) -> Value {
        let request_id = message.get("request_id").cloned().unwrap_or(Value::Null);
        let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
        let result = match kind {
            "vosu-settings-load" => self.load(message),
            "vosu-settings-save" => self.save(message),
            "vosu-profile-action" => self.profile_action(message),
            _ => Err(anyhow::anyhow!("unknown vosu settings request")),
        };
        match result {
            Ok(mut envelope) => {
                envelope["request_id"] = request_id;
                envelope
            }
            Err(error) => {
                let mut envelope = json!({
                    "type": "vosu-settings-error",
                    "request_id": request_id,
                    "message": format!("{error:#}"),
                });
                if let Some(ReadOnly(reason)) = error.downcast_ref::<ReadOnly>() {
                    envelope["read_only"] = reason.as_str().into();
                }
                envelope
            }
        }
    }

    fn rejected(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.rejected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The http(s) link of a `button` setting, which the editor opens in the browser. vosu's
    /// schema decides the link, never the page.
    pub(crate) fn button_link(&self, message: &Value) -> Result<Url> {
        let (counter, profile) = target(message)?;
        let id = message
            .get("unique_id")
            .and_then(Value::as_str)
            .context("missing setting identifier")?;
        let document = self.settings(counter, profile)?;
        let setting = document["settings"]
            .as_array()
            .and_then(|settings| {
                settings
                    .iter()
                    .find(|setting| setting["uniqueID"] == id && setting["type"] == "button")
            })
            .context("no such button setting")?;
        // A saved value replaces the schema's default link.
        let link = document["values"]
            .get(id)
            .unwrap_or(&setting["value"])
            .as_str()
            .context("the button has no link")?;
        let url = Url::parse(link).context("the button link is not a URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https"),
            "only http and https links open"
        );
        Ok(url)
    }

    fn load(&self, message: &Value) -> Result<Value> {
        let (counter, profile) = target(message)?;
        let document = self.settings(counter, profile)?;
        let registry = fetch_vosu_registry(&self.backend, self.timeout)?;
        Ok(json!({
            "type": "vosu-settings",
            "counter_id": counter,
            "profile": profile,
            "settings": document["settings"],
            "values": document["values"],
            "registry": registry,
        }))
    }

    fn save(&self, message: &Value) -> Result<Value> {
        let (counter, profile) = target(message)?;
        let values = message
            .get("values")
            .and_then(Value::as_array)
            .context("missing values")?;
        ensure!(
            values.iter().all(|entry| {
                entry.get("uniqueID").and_then(Value::as_str).is_some()
                    && entry.get("value").is_some()
            }),
            "each value needs a uniqueID and a value"
        );
        self.write(
            &settings_url(&self.backend, counter, profile)?,
            &Value::from(values.clone()),
        )?;
        Ok(json!({"type": "vosu-settings-saved", "counter_id": counter, "profile": profile}))
    }

    fn profile_action(&self, message: &Value) -> Result<Value> {
        let name = |key: &str| -> Result<Option<&str>> {
            match message.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => {
                    let name = value.as_str().filter(|name| valid_vosu_profile(name));
                    name.map(Some)
                        .with_context(|| format!("invalid profile name in {key}"))
                }
            }
        };
        let action = message
            .get("action")
            .and_then(Value::as_str)
            .context("missing profile action")?;
        let profile = name("name")?.context("missing profile name")?;
        let mut body = json!({"action": action, "name": profile});
        match action {
            "create" => {
                if let Some(source) = name("copy_from")? {
                    body["copyFrom"] = source.into();
                }
            }
            "rename" => {
                body["target"] = name("target")?.context("missing new profile name")?.into()
            }
            "delete" | "activate" => {}
            _ => bail!("unsupported profile action"),
        }
        let reply = self.write(&self.backend.join("/api/counters/profiles")?, &body)?;
        let registry = parse_vosu_registry(&serde_json::to_vec(&reply)?)?;
        Ok(json!({"type": "vosu-profiles", "registry": registry}))
    }

    fn settings(&self, counter: &str, profile: Option<&str>) -> Result<Value> {
        let body = get_json(
            &settings_url(&self.backend, counter, profile)?,
            self.timeout,
            MAX_SETTINGS_BYTES,
        )?;
        let document: Value =
            serde_json::from_slice(&body).context("vosu settings are not JSON")?;
        if let Some(error) = document.get("error") {
            bail!("vosu: {}", error.as_str().unwrap_or("request failed"));
        }
        ensure!(
            document["settings"].is_array() && document["values"].is_object(),
            "not vosu overlay settings"
        );
        Ok(document)
    }

    /// POSTs `body` with the broker token. A rejected token is read again once (vosu restarted);
    /// if vosu refuses that one too, it is remembered and writes stay off until the file changes.
    fn write(&self, endpoint: &Url, body: &Value) -> Result<Value> {
        let encoded = serde_json::to_vec(body)?;
        ensure!(encoded.len() <= MAX_WRITE_BYTES, "settings are too large");
        self.write_access().map_err(ReadOnly)?;
        let credential = self
            .credential
            .as_ref()
            .ok_or_else(|| ReadOnly(REMOTE.to_owned()))?;
        let mut token = credential.status().map_err(ReadOnly)?;
        for first in [true, false] {
            let mut response = ureq::post(endpoint.as_str())
                .header("Authorization", &format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .config()
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_global(Some(self.timeout))
                .build()
                .send(&encoded[..])
                .with_context(|| format!("failed to reach vosu at {endpoint}"))?;
            if response.status() == 401 {
                if first {
                    // Without a readable token there is nothing to retry with; say why.
                    let Some(fresh) = credential.reread() else {
                        return Err(ReadOnly(credential.status().err().unwrap_or_default()).into());
                    };
                    token = fresh;
                }
                continue;
            }
            ensure!(
                response.status().is_success(),
                "vosu refused the change (HTTP {})",
                response.status()
            );
            *self.rejected() = None;
            let reply = response
                .body_mut()
                .with_config()
                .limit(MAX_REPLY_BYTES as u64)
                .read_to_vec()
                .context("failed to read vosu's reply")?;
            let reply: Value =
                serde_json::from_slice(&reply).context("vosu's reply is not JSON")?;
            // vosu reports failures as HTTP 200 `{"error": ...}`.
            if let Some(error) = reply.get("error") {
                bail!("vosu: {}", error.as_str().unwrap_or("request failed"));
            }
            return Ok(reply);
        }
        *self.rejected() = Some(token);
        Err(ReadOnly(REJECTED.to_owned()).into())
    }
}

/// The message's overlay and optional profile, validated.
fn target(message: &Value) -> Result<(&str, Option<&str>)> {
    let counter = message
        .get("counter_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
        .context("missing overlay")?;
    let profile = match message.get("profile") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|name| valid_vosu_profile(name))
                .context("invalid vosu profile name")?,
        ),
    };
    Ok((counter, profile))
}

fn settings_url(backend: &Url, counter: &str, profile: Option<&str>) -> Result<Url> {
    let mut url = backend.join("/api/counters/settings/")?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("invalid vosu URL"))?
        .pop_if_empty()
        .push(counter);
    if let Some(profile) = profile {
        url.query_pairs_mut().append_pair("profile", profile);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    const TOKEN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const RESTARTED: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[derive(Clone, Debug)]
    struct Seen {
        method: String,
        target: String,
        authorization: Option<String>,
        body: String,
    }

    /// A vosu stand-in: answers each request with `respond`, and refuses writes whose bearer
    /// token is not `accepted` with 401.
    struct FakeVosu {
        url: Url,
        seen: Arc<Mutex<Vec<Seen>>>,
        accepted: Arc<Mutex<String>>,
    }

    impl FakeVosu {
        fn start(respond: fn(&Seen) -> String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let accepted = Arc::new(Mutex::new(TOKEN.to_owned()));
            let (record, token) = (seen.clone(), accepted.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let (method, target) = (
                        parts.next().unwrap().to_owned(),
                        parts.next().unwrap().to_owned(),
                    );
                    let (mut authorization, mut length) = (None, 0);
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        let header = header.trim_end();
                        if header.is_empty() {
                            break;
                        }
                        let (name, value) = header.split_once(':').unwrap();
                        match name.to_ascii_lowercase().as_str() {
                            "authorization" => authorization = Some(value.trim().to_owned()),
                            "content-length" => length = value.trim().parse().unwrap(),
                            _ => {}
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let request = Seen {
                        method,
                        target,
                        authorization,
                        body: String::from_utf8(body).unwrap(),
                    };
                    record.lock().unwrap().push(request.clone());
                    let (status, reply) = if request.method == "POST"
                        && request.authorization.as_deref()
                            != Some(&format!("Bearer {}", token.lock().unwrap()))
                    {
                        ("401 Unauthorized", String::new())
                    } else {
                        ("200 OK", respond(&request))
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                }
            });
            Self {
                url,
                seen,
                accepted,
            }
        }

        fn client(&self, token: Option<&str>) -> (VosuClient, Option<tempfile::TempDir>) {
            let Some(token) = token else {
                return (
                    VosuClient::with_credential(self.url.clone(), None, Duration::from_secs(2)),
                    None,
                );
            };
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("broker.json");
            write_token(&path, &self.url, token);
            let credential = BrokerCredential::at(path, &self.url);
            (
                VosuClient::with_credential(self.url.clone(), credential, Duration::from_secs(2)),
                Some(directory),
            )
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn write_token(path: &std::path::Path, vosu: &Url, token: &str) {
        let origin = vosu.origin().ascii_serialization();
        std::fs::write(
            path,
            format!(r#"{{"version":1,"origin":"{origin}","token":"{token}"}}"#),
        )
        .unwrap();
    }

    fn vosu(request: &Seen) -> String {
        if request.target.starts_with("/api/counters/profiles") {
            r#"{"active":"default","profiles":["default","stream"]}"#.into()
        } else if request.method == "POST" {
            r#"{"result":"success"}"#.into()
        } else {
            r#"{"settings":[{"uniqueID":"site","type":"button","value":"https://example.com/help"},{"uniqueID":"js","type":"button","value":"javascript:alert(1)"},{"uniqueID":"size","type":"number","value":3}],"values":{"size":5}}"#.into()
        }
    }

    #[test]
    fn loads_a_profiles_settings_and_the_registry_without_the_token() {
        let fake = FakeVosu::start(vosu);
        let (client, _token) = fake.client(Some(TOKEN));
        let reply = client.handle(&json!({
            "type": "vosu-settings-load", "request_id": "r1", "counter_id": "a/b c", "profile": "stream"
        }));
        assert_eq!(reply["type"], "vosu-settings", "{reply}");
        assert_eq!(reply["request_id"], "r1");
        assert_eq!(reply["values"], json!({"size": 5}));
        assert_eq!(
            reply["registry"],
            json!({"active": "default", "profiles": ["default", "stream"]})
        );
        let seen = fake.seen();
        assert_eq!(
            seen[0].target,
            "/api/counters/settings/a%2Fb%20c?profile=stream"
        );
        assert!(seen.iter().all(|request| request.authorization.is_none()));
    }

    #[test]
    fn saves_with_the_token_and_rereads_it_once_after_vosu_restarts() {
        let fake = FakeVosu::start(vosu);
        let (client, directory) = fake.client(Some(TOKEN));
        let save = json!({
            "type": "vosu-settings-save", "request_id": 7, "counter_id": "probe",
            "values": [{"uniqueID": "size", "value": 9}]
        });
        assert_eq!(client.handle(&save)["type"], "vosu-settings-saved");
        let first = fake.seen().pop().unwrap();
        assert_eq!(first.target, "/api/counters/settings/probe");
        assert_eq!(
            first.authorization.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(first.body, r#"[{"uniqueID":"size","value":9}]"#);

        // vosu restarted with a new token: the cached one is refused once, then the file is reread.
        *fake.accepted.lock().unwrap() = RESTARTED.to_owned();
        write_token(
            &directory.as_ref().unwrap().path().join("broker.json"),
            &fake.url,
            RESTARTED,
        );
        assert_eq!(client.handle(&save)["type"], "vosu-settings-saved");
        let tokens: Vec<_> = fake
            .seen()
            .into_iter()
            .skip(1)
            .map(|request| request.authorization)
            .collect();
        assert_eq!(
            tokens,
            [
                Some(format!("Bearer {TOKEN}")),
                Some(format!("Bearer {RESTARTED}"))
            ]
        );

        // Refused again after the reread: read-only with the reason, not a third attempt.
        *fake.accepted.lock().unwrap() = "f".repeat(64);
        let reply = client.handle(&save);
        assert_eq!(
            (reply["type"].as_str(), reply["request_id"].as_i64()),
            (Some("vosu-settings-error"), Some(7))
        );
        assert_eq!(reply["read_only"], REJECTED);
        assert_eq!(fake.seen().len(), 5);
        // The refused token stays refused: the catalog reports why, and saves send nothing.
        assert_eq!(client.write_access(), Err(REJECTED.to_owned()));
        assert_eq!(client.handle(&save)["read_only"], REJECTED);
        assert_eq!(fake.seen().len(), 5);
        // A new token in the file (the editor's retry) makes vosulay writable again.
        let fixed = "f".repeat(64);
        write_token(
            &directory.as_ref().unwrap().path().join("broker.json"),
            &fake.url,
            &fixed,
        );
        assert_eq!(client.write_access(), Ok(()));
        assert_eq!(client.handle(&save)["type"], "vosu-settings-saved");
    }

    #[test]
    fn without_a_token_nothing_is_written_and_the_reason_is_reported() {
        let fake = FakeVosu::start(vosu);
        let (remote, _) = fake.client(None);
        let save = json!({
            "type": "vosu-settings-save", "counter_id": "probe", "values": [{"uniqueID": "size", "value": 1}]
        });
        assert_eq!(remote.write_access(), Err(REMOTE.to_owned()));
        let reply = remote.handle(&save);
        assert_eq!(reply["type"], "vosu-settings-error");
        assert_eq!(reply["read_only"], REMOTE);

        let (missing, directory) = fake.client(Some(TOKEN));
        std::fs::remove_file(directory.as_ref().unwrap().path().join("broker.json")).unwrap();
        let reason = missing.write_access().unwrap_err();
        assert!(reason.contains("not shared a token"), "{reason}");
        assert_eq!(missing.handle(&save)["read_only"], reason.as_str());
        assert!(fake.seen().is_empty());
    }

    #[test]
    fn profile_actions_map_to_vosus_api_and_reject_bad_names_first() {
        let fake = FakeVosu::start(vosu);
        let (client, _token) = fake.client(Some(TOKEN));
        let reply = client.handle(&json!({
            "type": "vosu-profile-action", "request_id": "c", "action": "create", "name": "live", "copy_from": "stream"
        }));
        assert_eq!(reply["type"], "vosu-profiles", "{reply}");
        assert_eq!(reply["registry"]["profiles"], json!(["default", "stream"]));
        let rename = json!({"type": "vosu-profile-action", "action": "rename", "name": "live", "target": "show"});
        assert_eq!(client.handle(&rename)["type"], "vosu-profiles");
        let bodies: Vec<Value> = fake
            .seen()
            .iter()
            .map(|request| serde_json::from_str(&request.body).unwrap())
            .collect();
        assert_eq!(
            bodies,
            [
                json!({"action": "create", "name": "live", "copyFrom": "stream"}),
                json!({"action": "rename", "name": "live", "target": "show"}),
            ]
        );
        for bad in [
            json!({"type": "vosu-profile-action", "action": "create", "name": "bad name"}),
            json!({"type": "vosu-profile-action", "action": "rename", "name": "live"}),
            json!({"type": "vosu-profile-action", "action": "copyOverlay", "name": "live"}),
            json!({"type": "vosu-settings-save", "counter_id": "probe", "profile": "../x", "values": []}),
        ] {
            assert_eq!(client.handle(&bad)["type"], "vosu-settings-error", "{bad}");
        }
        assert_eq!(fake.seen().len(), 2);
    }

    #[test]
    fn button_links_come_from_vosus_schema_and_open_only_on_the_web() {
        let fake = FakeVosu::start(vosu);
        let (client, _token) = fake.client(Some(TOKEN));
        let link = |id: &str| client.button_link(&json!({"counter_id": "probe", "unique_id": id}));
        assert_eq!(link("site").unwrap().as_str(), "https://example.com/help");
        assert!(link("js").is_err());
        // Not a button.
        assert!(link("size").is_err());
    }
}
