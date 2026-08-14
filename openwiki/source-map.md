---
type: Source Map
title: vosulay source map
description: Change-oriented map for the shared Electron host, Windows asdf provider, Linux Rust native backend, packaging, and documentation.
tags: [source-map, navigation, maintenance, vosulay]
---

# vosulay source map

Use the [architecture overview](architecture/overview.md) for relationships and this page for the first file to open when making a change.

| Area | Start here | Responsibility |
|---|---|---|
| Application lifecycle | [`desktop/src/main/index.ts`](../desktop/src/main/index.ts) | Single instance, shared services, remote server, tray, global shortcuts, provider startup, and ordered shutdown. |
| Platform selection | [`desktop/src/main/provider-factory.ts`](../desktop/src/main/provider-factory.ts), [`desktop/src/main/overlay-provider.ts`](../desktop/src/main/overlay-provider.ts) | Dynamically loads only the current platform provider and defines the shared provider contract. |
| Attachment reconciliation | [`desktop/src/main/overlay-manager.ts`](../desktop/src/main/overlay-manager.ts) | PID discovery, focus selection, serialized attach/detach, editing, visibility, reload, save flush, and stop behavior. |
| Windows in-game provider | [`desktop/src/main/windows-overlay-provider.ts`](../desktop/src/main/windows-overlay-provider.ts), [`desktop/src/main/overlay-process.ts`](../desktop/src/main/overlay-process.ts) | asdf-overlay attachment, shared GPU surface, Windows input redirection, and active-window tracking. |
| Linux provider | [`desktop/src/main/linux-overlay-provider.ts`](../desktop/src/main/linux-overlay-provider.ts) | Native addon loading, late-attach options, Vulkan launch, and native handle adaptation. |
| Linux Electron OSR | [`desktop/src/main/linux-overlay-process.ts`](../desktop/src/main/linux-overlay-process.ts) | Hidden BrowserWindow, target sizing, DMA-BUF forwarding/release, native events, and Electron/CDP input dispatch. |
| Configuration | [`desktop/src/main/config.ts`](../desktop/src/main/config.ts), [`desktop/src/main/storage-path.ts`](../desktop/src/main/storage-path.ts) | Version 1 normalization, revisions, atomic persistence, platform and portable paths. See [configuration](domain/configuration.md). |
| Manager/native bridge | [`desktop/src/main/host-bridge.ts`](../desktop/src/main/host-bridge.ts), [`desktop/src/preload/index.ts`](../desktop/src/preload/index.ts) | Load/save revisions, editing state, counter refresh, custom overlay selection, import/export, and renderer messaging. |
| vosu catalog | [`desktop/src/main/counters.ts`](../desktop/src/main/counters.ts), [`desktop/src/main/urls.ts`](../desktop/src/main/urls.ts) | Bounded HTML fetch/parsing and safe counter/settings URL construction. |
| Local overlay capabilities | [`desktop/src/main/local-overlays.ts`](../desktop/src/main/local-overlays.ts) | File selection, canonical roots, opaque custom scheme, MIME serving, and path confinement. |
| Remote profile server | [`desktop/src/main/remote-server.ts`](../desktop/src/main/remote-server.ts) | Credential-protected view-only pages, authority/origin/fetch-metadata checks, peer allowlist, request bounds, and opaque local capability forwarding. |
| Manager document | [`desktop/src/main/manager-document.ts`](../desktop/src/main/manager-document.ts), [`assets/manager.html`](../assets/manager.html), [`assets/manager.css`](../assets/manager.css), [`assets/manager.js`](../assets/manager.js) | Safe bootstrap document plus profile rendering and edit UI. |
| Linux wire protocol | [`native/crates/overlay-protocol/src/lib.rs`](../native/crates/overlay-protocol/src/lib.rs) | Explicit bounded packet codecs, abstract seqpacket sockets, credentials, target/frame/input messages, and FD validation. |
| Linux host session | [`native/crates/overlay-host/src/lib.rs`](../native/crates/overlay-host/src/lib.rs) | Random endpoint/cookie, authenticated handshake, target lifecycle, frame ownership, and host events. |
| Node-API addon | [`native/crates/overlay-node/src/lib.rs`](../native/crates/overlay-node/src/lib.rs) | Electron-facing attach/surface/session APIs, event pump, profiler attach, and Vulkan child launcher. |
| Agent state | [`native/crates/overlay-agent-core/src/lib.rs`](../native/crates/overlay-agent-core/src/lib.rs) | Fail-open authenticated connection, latest-frame mailbox, bounded input queue, capture watchdog, and disconnect behavior. |
| GL compositor/input | [`native/crates/overlay-agent-gl/src/lib.rs`](../native/crates/overlay-agent-gl/src/lib.rs) | GLX/EGL/SDL hooks, SHM/DMA-BUF import, state restoration, composition, and SDL input capture. |
| Vulkan compositor | [`native/crates/overlay-vulkan-layer/src/lib.rs`](../native/crates/overlay-vulkan-layer/src/lib.rs), [`native/crates/overlay-vulkan-layer/src/compositor.rs`](../native/crates/overlay-vulkan-layer/src/compositor.rs) | Loader negotiation, Vulkan object/swapchain tracking, external-memory import, synchronization, and present-time composition. |
| .NET late attach | [`native/crates/overlay-dotnet-bootstrap/src/lib.rs`](../native/crates/overlay-dotnet-bootstrap/src/lib.rs), [`native/crates/overlay-dotnet-bootstrap/dotnet/Vosulay.AttachProfiler/Program.cs`](../native/crates/overlay-dotnet-bootstrap/dotnet/Vosulay.AttachProfiler/Program.cs) | Minimal CLR profiler bootstrap and DiagnosticsClient attach tool for native lazer. |
| Linux native staging | [`desktop/scripts/stage-native.mjs`](../desktop/scripts/stage-native.mjs), [`native/VkLayer_VOSULAY_overlay.json`](../native/VkLayer_VOSULAY_overlay.json) | Builds and stages addon, agents, profiler tool, Vulkan layer, and manifest outside ASAR. |
| Packaging validation | [`desktop/package.json`](../desktop/package.json), [`desktop/scripts/verify-package.mjs`](../desktop/scripts/verify-package.mjs) | Platform-specific artifact composition, required/forbidden resources, executable attach tool, and canonical package identity. |
| Documentation maintenance | [`openwiki/quickstart.md`](quickstart.md), [`README.md`](../README.md) | Manual/local OpenWiki updates; no OpenWiki GitHub Actions workflow. |

## Practical change paths

- **Add or change a configuration field:** update `desktop/src/main/config.ts`, shared types, manager UI handling, and relevant TypeScript tests. Update the Rust-derived golden fixture only when the intentional observable schema changes.
- **Change counter discovery:** update `desktop/src/main/counters.ts`, URL helpers, fixtures, and service tests; preserve deadline, response-size, content-type, ordering, and encoding contracts.
- **Change attachment selection:** update `overlay-manager.ts` and platform-specific provider/process seams. Keep platform-native imports behind `provider-factory.ts`.
- **Change Linux protocol:** update `overlay-protocol`, both host and agent consumers, codec/FD/authentication tests, addon declarations, and any TypeScript event parser. Major incompatibility requires a protocol-major bump.
- **Change a compositor:** preserve original presentation, GL/Vulkan state, bounded frame ownership, and disconnect fail-open behavior; smoke-test the real target/backend.
- **Change Linux package contents:** update `stage-native.mjs`, `package.json`, and `verify-package.mjs` together, then build the AppImage and inspect the packaged resources.
- **Change local or remote overlay security:** update the canonical TypeScript service and its tests for path confinement, capability opacity, credentials, authority, peer allowlist, browser metadata, sandbox, and response headers.
