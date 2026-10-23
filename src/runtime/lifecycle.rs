//! Reuse vosux's native broker authority; never discover listeners by probing ports.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::time::{Duration, Instant};
use url::Url;

const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(750);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(3);
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(5);

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build(),
    )
}

fn enabled(value: &Value) -> bool {
    value["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["id"] == "vosulay" && tool["enabled"] == true && tool["ui"] == true)
    })
}

/// Once a live vosu confirms it owns this enabled tool, never start another host,
/// even if opening its editor subsequently fails. No broker/unreachable/disabled
/// leaves the existing standalone path untouched.
pub(super) fn handoff(open_browser: bool) -> bool {
    let Some((origin, token)) = crate::backend::local_host() else {
        return false;
    };
    let client = agent(LAUNCH_TIMEOUT);
    let authorization = format!("Bearer {token}");
    let discovered = (|| -> Result<bool> {
        let mut reply = client
            .get(origin.join("tools/_host/v1/cli")?.as_str())
            .header("Authorization", &authorization)
            .config()
            .timeout_global(Some(DISCOVERY_TIMEOUT))
            .build()
            .call()?;
        if reply.status() != 200 {
            return Ok(false);
        }
        let bytes = reply
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()?;
        let value: Value = serde_json::from_slice(&bytes)?;
        Ok(enabled(&value))
    })();
    if !matches!(discovered, Ok(true)) {
        return false;
    }
    match launch(&client, &origin, &authorization) {
        Ok(url) => {
            // A launch fragment is a short-lived write capability: never print it.
            let mut public = url.clone();
            public.set_fragment(None);
            eprintln!("Vosulay is managed by vosu: {public}");
            if open_browser && let Err(error) = open::that_detached(url.as_str()) {
                eprintln!(
                    "Vosulay: could not open its vosu editor: {error}; open Tools in vosu's dashboard."
                );
            }
        }
        Err(error) => eprintln!(
            "Vosulay is managed by vosu; could not open its editor: {error:#}. Use Start/Open in vosu's dashboard."
        ),
    }
    true
}

fn launch(client: &ureq::Agent, origin: &Url, authorization: &str) -> Result<Url> {
    let mut reply = client
        .post(origin.join("tools/_host/v1/cli/vosulay/open")?.as_str())
        .header("Authorization", authorization)
        .header("Content-Type", "application/json")
        .send(&b"{}"[..])?;
    ensure!(
        reply.status() == 200,
        "vosu refused the editor launch (HTTP {})",
        reply.status()
    );
    let bytes = reply
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_vec()?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let url = Url::parse(
        value["url"]
            .as_str()
            .context("vosu returned no editor URL")?,
    )?;
    ensure!(
        url.origin() == origin.origin()
            && url.path() == "/tools/vosulay/"
            && url.username().is_empty()
            && url.password().is_none(),
        "vosu returned an unexpected editor URL"
    );
    Ok(url)
}

pub(super) fn open_dashboard() {
    let result = (|| -> Result<()> {
        let (origin, _) =
            crate::backend::local_host().context("vosu's native broker is unavailable")?;
        let dashboard = crate::api::dashboard_origin(&origin, DISCOVERY_TIMEOUT)
            .context("vosu did not report its dashboard address")?;
        ensure!(
            dashboard
                .host_str()
                .and_then(|host| host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok())
                .is_some_and(|ip| ip.is_loopback()),
            "vosu reported a non-loopback dashboard"
        );
        open::that_detached(dashboard.join("/tools")?.as_str())?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Vosulay: could not open vosu's dashboard: {error:#}");
    }
}

pub(super) fn acquire(tool_mode: bool) -> Result<Option<super::instance::InstanceGuard>> {
    if let Some(guard) = super::instance::InstanceGuard::acquire()? {
        return Ok(Some(guard));
    }
    if !tool_mode {
        return Ok(None);
    }
    // Unlike Quit, this command is accepted only by a standalone host. The
    // existing channel authenticates the local user; no process is killed.
    super::control::send(super::control::Command::YieldToVosu)
        .context("cannot take ownership from the existing vosulay; close the standalone instance and Start in vosu")?;
    let until = Instant::now() + TAKEOVER_TIMEOUT;
    loop {
        if let Some(guard) = super::instance::InstanceGuard::acquire()? {
            return Ok(Some(guard));
        }
        ensure!(
            Instant::now() < until,
            "standalone vosulay did not release ownership within five seconds; close it and Start in vosu"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_an_enabled_installed_ui_is_owned_by_vosu() {
        assert!(enabled(
            &serde_json::json!({"tools":[{"id":"vosulay","enabled":true,"ui":true}]})
        ));
        for tools in [
            serde_json::json!([]),
            serde_json::json!([{"id":"vosulay","enabled":false,"ui":true}]),
            serde_json::json!([{"id":"other","enabled":true,"ui":true}]),
        ] {
            assert!(!enabled(&serde_json::json!({"tools":tools})));
        }
    }
}
