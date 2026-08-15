---
type: Testing Guide
title: vosulay testing guidance
description: Maps Electron, native protocol, compositor, packaging, and live target verification for the cross-platform in-game architecture.
resource: desktop/test/services.test.ts
tags: [testing, electron, rust, protocol, graphics, security]
---

# vosulay testing guidance

## Test layers

Use the smallest layer that proves the changed contract, but do not treat protocol/unit success as proof of in-game graphics behavior.

1. **TypeScript services and orchestration:** deterministic Node tests under `desktop/test/`.
2. **Rust protocol/host/agent state:** crate unit tests in `native/crates/*/src/`.
3. **Node-API integration:** real addon plus fake agent in `native/test/overlay-node.integration.test.mjs`.
4. **Packaging:** platform artifact build plus `desktop/scripts/verify-package.mjs`.
5. **Live compositor/input:** real native lazer or Vulkan/DXVK target.

## Electron checks

```sh
cd desktop
bun run typecheck
bun run test
bun run build
```

Current test ownership:

- [`services.test.ts`](../../desktop/test/services.test.ts): storage paths; option parsing; target selection; edit shortcuts; config normalization/revisions/atomic writes; manager document bootstrap; bounded catalog fetch/parsing; local-overlay canonicalization and path confinement; HostBridge message and save behavior.
- [`config-parity.test.ts`](../../desktop/test/config-parity.test.ts): canonical TypeScript normalization against the retained Rust-derived golden fixture and startup SharedArrayBuffer switch behavior.
- [`remote-server.test.ts`](../../desktop/test/remote-server.test.ts): peer allowlist, exact authority, DNS-rebinding resistance, credential routes, Origin/Fetch Metadata, profile selection/rendering, local capability forwarding, sandbox/header behavior, and admission limits.
- [`overlay-manager.test.ts`](../../desktop/test/overlay-manager.test.ts): PID discovery/selection, last-focused target, serialized detach/attach, stale attach disposal, state propagation, frame rate, flush/reload, and stop races.
- [`provider-factory.test.ts`](../../desktop/test/provider-factory.test.ts): platform dynamic import boundary, unsupported platforms, Linux addon selection, and authenticated Vulkan launch handoff.
- [`linux-overlay-process.test.ts`](../../desktop/test/linux-overlay-process.test.ts): Electron shared-texture metadata validation, target lifecycle parsing, SDL input validation, and Electron/CDP event mapping.
- [`foreground-tracker.test.ts`](../../desktop/test/foreground-tracker.test.ts): foreground-window transitions used by the Windows provider.

When changing an exported TypeScript service, run typecheck plus the full deterministic Node suite; tests are fast and share important fixtures.

## Native checks

```sh
cd native
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
bun install
bun run build
```

The Rust tests are colocated with implementation. Important protected behavior includes:

- `overlay-protocol`: packet/header codecs, version negotiation, payload and dimensional bounds, texture plane validation, sequence handling, credential retrieval, ancillary FD count/type validation, truncation and disconnect behavior.
- `overlay-host`: good/bad cookie, expected UID/PID/process-group authentication, handshake deadlines/version/capabilities, target lifecycle, frame ownership/consumption, clear/input/shutdown, reconnect/host death behavior.
- `overlay-agent-core`: fail-open EOF, latest-frame replacement and acknowledgement, bounded queues, visibility/capture commands, input capture watchdog, and no stale capture after disconnect.
- `overlay-agent-gl`: GL state guard coverage, SHM/DMA-BUF validation and import rejection, BGRA frame acceptance, `dlsym` resolution and interposed entry points, presenting-only session claim, SDL hook binding, input normalization/blocking, and panic containment helpers.
- `overlay-vulkan-layer`: loader negotiation, proc-address dispatch, instance/device/swapchain bookkeeping, external-memory identity/cache, synchronization and cleanup helpers.
- `overlay-node`: option validation, resource/environment construction, profiler attach command, Vulkan process-group launch, and event translation.
- `overlay-dotnet-bootstrap`: profiler callback/lifecycle contract and agent-loading argument validation.

A graphics-hook test must continue to assert fail-open behavior: the original swap/present remains callable after invalid descriptors, import failures, host EOF, panics, and unsupported formats.

## Node-API fake-agent integration

Build the real addon and fake agent, then provide both paths:

```sh
cd native
cargo build -p overlay-agent-core --bin overlay-fake-agent
bun run build
VOSULAY_NATIVE_ADDON="$PWD/crates/overlay-node/overlay-node.linux-x64-gnu.node" \
VOSULAY_FAKE_AGENT="$PWD/target/debug/overlay-fake-agent" \
bun run test
```

This exercises a real JavaScript `attach()` call, generated endpoint/cookie transfer through a test attach tool, authenticated host/agent connection, N-API event delivery, visibility/editing commands, and destroy/disconnect. The package script still invokes Node's built-in test runner; Bun owns installation and dispatch. Do not skip it when changing exported addon methods, host handshake state, or event names.

## Protocol/security matrix

For protocol or local-boundary changes, explicitly retain tests for:

- bad cookie, wrong PID, wrong UID where testable, wrong authenticated process group, incompatible major version, and capability mismatch;
- short/truncated packet, excessive or missing FDs, unexpected ancillary types, payload-size overflow, plane-count/stride/offset/size overflow, and unsupported format/modifier;
- host and agent death, reconnect refusal, shutdown, target add/resize/destroy, and automatic capture release;
- remote credential absence/mismatch, malicious Host/DNS rebinding, disallowed peer, cross-site Origin/Fetch Metadata, opaque sandbox exception only on local capability routes, and credential rotation;
- local symlink escape, non-canonical root/token, encoded dot/slash/backslash/NUL segments, non-file targets, and token/root rebinding.

## Package verification

Linux:

```sh
cd desktop
bun run dist:linux
```

Windows x64:

```powershell
cd desktop
bun run dist:win
```

The final command in each script runs `verify:package`. It checks non-empty distributables, package identity, manager assets/preload, required same-platform native resources, and forbidden opposite-platform resources. Linux additionally checks that the profiler attach tool is executable. A successful `bun run build` alone does not prove package completeness.

## Live native lazer matrix

Use a real target and enable `VOSULAY_NATIVE_DIAGNOSTICS=1` for first-frame evidence. Verify:

1. Host starts without Qt or desktop overlay surfaces.
2. Explicit PID or native process discovery attaches the CLR profiler/GL agent.
3. Target dimensions resize the hidden BrowserWindow.
4. Manager/counters render inside the game.
5. Rapid resize/fullscreen and renderer reload preserve game presentation.
6. Visibility toggles without detaching the game.
7. Edit mode captures SDL mouse/keyboard, sends controls to Electron, and blocks them from the game only while editing.
8. Leaving edit, hiding, host termination, or socket failure immediately restores game input.
9. Closing game first and closing host first both clean up without a hung present path.
10. A rejected DMA-BUF or malformed frame does not stop the game.
11. `vosulay target transport: ...` reflects the target's context, and a GLX target composites through shared memory.

Evidence should include the attach/target/forwarded/consumed log sequence plus visual confirmation of the in-game surface. Compositor screenshots are unreliable for fullscreen or nested sessions, so prefer the agent's `pre`/`post` frame dumps: they read back the target's own framebuffer. Native lazer on X11 negotiates shared memory because SDL creates a GLX context there.

## Live Vulkan/DXVK matrix

Launch through vosulay; attaching after Vulkan instance creation is not an equivalent test.

1. Loader negotiates and creates the layer before the game instance.
2. One session authenticates only the launched PID/process group.
3. Target add/resize and first frame composition occur.
4. Game presentation survives resize, swapchain recreation, visibility toggles, renderer reload, and invalid external-memory import.
5. Multiple swapchains/devices do not share one incorrect global compositor.
6. Host exit removes overlay work while the game continues or exits normally.
7. The layer remains dormant in a process without a valid vosulay session.

Current limitation: Vulkan/Wine input capture is not the GL SDL input path. Do not report the Vulkan live matrix as proof of edit-mode input redirection.

## Stable osu! on Wine

Stable validation needs a Wine prefix in which `osu!.exe` reaches its render loop without vosulay. An osu!winello installation provides one: `wine-osu`, a prefix with real .NET 4.x and no Wine Mono, and the game directory. A prefix that still loads Wine Mono crashes before the agent connects and reports `overlay agent did not connect before attach deadline`; that is an environment blocker, not a compositor result.

Launch stable through the host so the agent is preloaded, then verify:

1. `vosulay GL agent interposed glXSwapBuffers` appears — Wine resolves GL through `dlsym`, so absence means the interposer never took effect.
2. `vosulay target transport: dmabuf=false bgra_shm=true` — a GLX context must negotiate shared memory.
3. `vosulay shared-memory compositor first frame: <w>x<h> format=2`.
4. The game keeps rendering: compare the `pre`/`post` frame dumps. A uniformly black `post` frame means the overlay erased the game and is a release blocker.
5. Overlay content lands upright and in the configured position. Gameplay counters render nothing on menus, so use a counter or URL overlay that draws while idle when checking placement.

### Guest VM procedure

The Linux gaming VM (`ssh -p 2222 vosu@127.0.0.1`) has no Wine of its own. Copy a working host installation into it, keeping the same relative paths, then repoint the prefix's `dosdevices/c:` and `d:` links at the guest home:

```sh
rsync -a -e 'ssh -p 2222' ~/.local/share/osuconfig/wine-osu/ vosu@127.0.0.1:/home/vosu/.local/share/osuconfig/wine-osu/
rsync -a -e 'ssh -p 2222' ~/.local/share/wineprefixes/osu-wineprefix/ vosu@127.0.0.1:/home/vosu/.local/share/wineprefixes/osu-wineprefix/
rsync -a -e 'ssh -p 2222' --exclude=Songs --exclude=Skins "$HOME/.local/share/osu-wine/osu!/" "vosu@127.0.0.1:/home/vosu/.local/share/osu-wine/osu!/"
```

The passed GPU has no connected display, so run the game inside `gamescope --backend headless -W 2560 -H 1440` and capture with `GAMESCOPE_WAYLAND_DISPLAY=<socket> gamescopectl screenshot <path>`; read the socket from the game process's environment. Counters need a vosu instance in the guest with the referenced overlays present under its static folder. Prefer the agent's frame dumps over compositor screenshots when a client's output is uniform: a black gamescope frame cannot distinguish "overlay erased the game" from "wrong window composited".

## Performance regression checks

July 2026 baseline (`/home/user/VMs/benchmarks-2026-07-26`, Qt desktop overlay, lazer 1280×720@60, 120 s × 3 paired trials) median ON−OFF deltas:

| metric | July Qt desktop |
| --- | ---: |
| full-stack CPU (one-core %-points) | +14.0 |
| private MiB | +412 |
| RSS MiB | +1122 |
| GPU util %-points | +2.52 |
| GPU memory MiB | +108 |

The Electron migration is not a drop-in re-run of that protocol: Linux composition moved in-process, Wine/stable GLX negotiates full-frame SHM rather than DMA-BUF, and default capture is 2560×1440. Treat numbers as architecture deltas, not A/B on identical code.

Hot-path invariants to re-check after transport changes:

1. DMA-BUF and bitmap paints both bound outstanding work (≤2). An unbounded bitmap path copies ~14 MiB per Chromium paint into sealed memfds.
2. `sealed_memfd` runs on the N-API worker, not the paint callback. At 2560×1440 the write alone is ~1.5–1.8 ms on this workstation — fine at 60 Hz once off the JS thread, fatal if it blocks `paint`.
3. Present hooks must not call `getenv` every frame; cache diagnostics/`VOSULAY_FRAME_DUMP_DELAY`.
4. SHM upload is full-frame `glTexSubImage2D` (no dirty rect yet). DMA-BUF reuse pools imported images by identity.
5. Windows still uses `@asdf-overlay/electron` `ElectronOverlaySurface.connect`; the provider split must not reintroduce a manual `paint`→CPU path there.

Smoke ON/OFF (Wine stable, gamescope headless 2560×1440@60, 20 s warmup / 40 s measure, order off/on/on/off, SHM compositor confirmed) after the bounds/worker-thread fixes:

| paired median ON−OFF | value |
| --- | ---: |
| full-stack CPU (one-core %-points) | +8.0 |
| private MiB | +242 |
| RSS MiB | +1089 |
| GPU util %-points | 0 |
| GPU memory MiB | +145 |
| GPU power W | +1.6 |

Not a release-gate protocol re-run (menu workload, shorter windows, different client/resolution). Relative to the July Qt desktop baseline it does **not** show a CPU or private-memory regression; RSS is comparable. Remaining cost is structural: full-frame SHM at 1440p until EGL contexts can take DMA-BUF, and Electron's multi-process RSS footprint.

## Documentation and generated files

OpenWiki is maintained manually and locally. After source changes, update only affected pages or run `openwiki code --update --print`, inspect the diff, and never add scheduled or manually dispatched OpenWiki GitHub Actions. Generated N-API declarations should be rebuilt through `bun run build`, not hand-edited as the source of truth.
