//! Minimal CoreCLR attach profiler that starts the native GL overlay agent.
//!
//! The profiler is only a bootstrap transport. Once the separately retained GL
//! agent has copied the attach payload, the profiler detaches itself so a later
//! DMA-BUF -> SHM fallback can use `AttachProfiler` again in the same process.

use clr_profiler::{
    ClrProfiler, CorProfilerCallback, CorProfilerCallback2, CorProfilerCallback3,
    CorProfilerCallback4, CorProfilerCallback5, CorProfilerCallback6, CorProfilerCallback7,
    CorProfilerCallback8, CorProfilerCallback9, CorProfilerInfo3, ProfilerInfo,
    ffi::{E_FAIL, HRESULT},
    register,
};
use std::{ffi::c_void, panic::catch_unwind, slice};
use uuid::Uuid;

pub const PROFILER_CLSID: Uuid = Uuid::from_bytes([
    0x60, 0xf0, 0xba, 0x7d, 0x3c, 0xe1, 0x4f, 0x69, 0x93, 0x26, 0x79, 0xa9, 0xe0, 0x21, 0xb4, 0xa8,
]);
const MAX_PAYLOAD_LEN: usize = 4096;
const DETACH_EXPECTED_COMPLETION_MS: u32 = 100;

type AgentStart = unsafe extern "C" fn(*const u8, usize) -> i32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachPayload {
    agent_path: Vec<u8>,
    agent_payload: Vec<u8>,
}

impl AttachPayload {
    fn parse(client_data: *const c_void, client_data_len: u32) -> Result<Self, HRESULT> {
        let len = client_data_len as usize;
        if len == 0 || len > MAX_PAYLOAD_LEN || client_data.is_null() {
            return Err(E_FAIL);
        }
        // SAFETY: CoreCLR supplies `client_data_len` readable bytes for this callback.
        let bytes = unsafe { slice::from_raw_parts(client_data.cast::<u8>(), len) };
        let fields: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
        if fields.len() != 5 || !fields[4].is_empty() {
            return Err(E_FAIL);
        }
        validate_agent_path(fields[0])?;
        let mut agent_payload = fields[1..4].join(&0);
        agent_payload.push(0);
        validate_agent_payload(&agent_payload)?;
        Ok(Self {
            agent_path: fields[0].to_vec(),
            agent_payload,
        })
    }
}

fn validate_agent_path(path: &[u8]) -> Result<(), HRESULT> {
    if path.is_empty() || path.len() > 3072 || path.contains(&0) || path[0] != b'/' {
        return Err(E_FAIL);
    }
    Ok(())
}

fn validate_agent_payload(payload: &[u8]) -> Result<(), HRESULT> {
    if payload.last() != Some(&0) {
        return Err(E_FAIL);
    }
    let fields: Vec<&[u8]> = payload[..payload.len() - 1]
        .split(|byte| *byte == 0)
        .collect();
    if fields.len() != 3
        || fields[0].is_empty()
        || fields[0].len() > 107
        || fields[1].len() != 64
        || !fields[1].iter().all(u8::is_ascii_hexdigit)
        || fields[2].is_empty()
        || fields[2].len() > 10
        || !fields[2].iter().all(u8::is_ascii_digit)
        || fields[2] == b"0"
    {
        return Err(E_FAIL);
    }
    Ok(())
}

fn agent_start(path: &[u8]) -> Result<AgentStart, HRESULT> {
    let path = std::ffi::CString::new(path).map_err(|_| E_FAIL)?;
    // SAFETY: `path` is NUL terminated and retained for the duration of `dlopen`.
    let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        return Err(E_FAIL);
    }
    // SAFETY: the GL agent exports this exact C ABI. This dlopen reference is
    // deliberately retained for process lifetime so the agent remains loaded
    // after CoreCLR detaches and unloads this bootstrap profiler.
    let symbol = unsafe { libc::dlsym(library, c"vosulay_agent_start_with_payload".as_ptr()) };
    if symbol.is_null() {
        return Err(E_FAIL);
    }
    Ok(unsafe { std::mem::transmute::<*mut c_void, AgentStart>(symbol) })
}

#[derive(Clone)]
struct VosulayAttachProfiler {
    /// Retained only until `ProfilerAttachComplete`, when CoreCLR has made the
    /// profiler active and `RequestProfilerDetach` is legal.
    profiler_info: Option<ProfilerInfo>,
}

impl ClrProfiler for VosulayAttachProfiler {
    fn new() -> Self {
        Self {
            profiler_info: None,
        }
    }

    fn clsid(&self) -> &Uuid {
        &PROFILER_CLSID
    }
}

impl CorProfilerCallback for VosulayAttachProfiler {}
impl CorProfilerCallback2 for VosulayAttachProfiler {}
impl CorProfilerCallback3 for VosulayAttachProfiler {
    fn initialize_for_attach(
        &mut self,
        profiler_info: ProfilerInfo,
        client_data: *const c_void,
        client_data_len: u32,
    ) -> Result<(), HRESULT> {
        let result = catch_unwind(|| {
            let payload = AttachPayload::parse(client_data, client_data_len)?;
            let start = agent_start(&payload.agent_path)?;
            // SAFETY: `payload` remains alive through the call; the agent copies it before return.
            let result =
                unsafe { start(payload.agent_payload.as_ptr(), payload.agent_payload.len()) };
            if result < 0 { Err(E_FAIL) } else { Ok(()) }
        })
        .unwrap_or(Err(E_FAIL));

        if result.is_ok() {
            self.profiler_info = Some(profiler_info);
        }
        result
    }

    fn profiler_attach_complete(&mut self) -> Result<(), HRESULT> {
        let Some(profiler_info) = self.profiler_info.take() else {
            return Ok(());
        };
        // The native GL agent is now independently resident and owns its own
        // control thread/hooks. Release CoreCLR's single active-profiler slot so
        // OverlayManager can perform a fresh attach if DMA-BUF later downgrades.
        profiler_info.request_profiler_detach(DETACH_EXPECTED_COMPLETION_MS)
    }
}
impl CorProfilerCallback4 for VosulayAttachProfiler {}
impl CorProfilerCallback5 for VosulayAttachProfiler {}
impl CorProfilerCallback6 for VosulayAttachProfiler {}
impl CorProfilerCallback7 for VosulayAttachProfiler {}
impl CorProfilerCallback8 for VosulayAttachProfiler {}
impl CorProfilerCallback9 for VosulayAttachProfiler {}

register!(VosulayAttachProfiler);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_attach_payload() {
        let payload = concat!(
            "/tmp/libagent.so\0",
            "vosulay-abc\0",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\0",
            "1234\0",
        )
        .as_bytes();
        let parsed = AttachPayload::parse(payload.as_ptr().cast(), payload.len() as u32).unwrap();
        assert_eq!(parsed.agent_path, b"/tmp/libagent.so");
        assert!(validate_agent_payload(&parsed.agent_payload).is_ok());
    }

    #[test]
    fn rejects_invalid_payloads() {
        assert!(validate_agent_payload(b"").is_err());
        assert!(validate_agent_payload(b"name\0abcd\0\x31").is_err());
        assert!(
            validate_agent_payload(
                concat!(
                    "name\0",
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\0",
                    "0\0",
                )
                .as_bytes(),
            )
            .is_err()
        );
        let relative_path = b"relative\0name\0cookie\0\x31\0".to_vec();
        assert!(
            AttachPayload::parse(
                relative_path.as_ptr().cast(),
                relative_path.len().try_into().unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn bootstrap_starts_without_profiler_info_until_attach_completes() {
        let profiler = VosulayAttachProfiler::new();
        assert!(profiler.profiler_info.is_none());
        assert_eq!(DETACH_EXPECTED_COMPLETION_MS, 100);
    }
}
