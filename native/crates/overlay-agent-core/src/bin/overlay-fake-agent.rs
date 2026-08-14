use overlay_agent_core::{Agent, AgentConfig};
use overlay_protocol::{Endpoint, Target};
use std::{env, thread, time::Duration};

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "invalid hex".to_string()))
        .collect()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let name = decode_hex(&args.next().ok_or("missing endpoint")?)?;
    let cookie = decode_hex(&args.next().ok_or("missing cookie")?)?;
    let cookie: [u8; 32] = cookie.try_into().map_err(|_| "cookie must be 32 bytes")?;
    let endpoint = Endpoint::from_name(&name)?;
    let mut config = AgentConfig::current();
    // Mirrors a shared-memory-only target such as a GLX game under Wine.
    config.advertised_capabilities = overlay_protocol::CAP_SHM | overlay_protocol::CAP_SHM_BGRA;
    let mut agent = Agent::connect(&endpoint, cookie, config)?;
    agent.send_target_create(Target {
        target_id: 1,
        width: 1280,
        height: 720,
    })?;
    agent.set_nonblocking()?;
    loop {
        match agent.poll() {
            Ok(_) => {}
            Err(overlay_agent_core::AgentError::Protocol(overlay_protocol::ProtocolError::Io(
                e,
            ))) if e.kind() == std::io::ErrorKind::WouldBlock => {
                agent.watchdog_tick();
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
        agent.watchdog_tick();
        if agent.is_disconnected() {
            break;
        }
    }
    Ok(())
}
