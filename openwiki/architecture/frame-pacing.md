---
type: Architecture
title: Frame pacing
description: Game-present-driven Linux overlay pacing, configured renderer ceilings, and the design trade-off relative to idk-overlay.
---

# Frame pacing

vosulay treats **composition cadence**, **renderer cadence**, and **GPU-buffer lifetime** as separate concerns.

The game is allowed to present as fast as it wants. The injected GL agent or Vulkan layer composites the newest completed overlay texture into every eligible game present, but Electron only produces a new overlay frame when the native backend reports demand and the configured FPS ceiling allows another update. JavaScript `requestAnimationFrame` callbacks inside the manager and every counter/custom-overlay iframe are independently capped to the same configured FPS.

The effective fresh-overlay rate is therefore bounded by:

```text
min(game present rate, configured vosulay FPS, Electron/overlay render capacity)
```

A game presenting at 1000 Hz with `--fps-limit 240` can still receive the latest overlay texture on all 1000 presents, while Chromium animation work and new OSR frames stay at or below roughly 240 Hz. Presents between fresh overlay frames reuse the most recently completed texture.

## Linux demand path

The Linux path is deliberately latest-wins:

```text
game GL/Vulkan present
        |
        | atomic/lock-free present observation only
        v
native control thread
        |
        | coalesce intermediate presents
        | send newest FrameRequest
        v
Electron main process
        |
        | coalesce requests
        | enforce configured FPS deadline
        v
startPainting() + invalidate()
        |
        v
one OSR paint
        |
        v
DMA-BUF (preferred) or SHM frame
        |
        v
native mailbox -> game presents reuse newest frame
```

The game presentation hook does not perform host socket I/O and does not wait for Electron. The GL/Vulkan control thread observes how far presentation demand advanced and forwards only the newest request sequence. The Electron pacing controller similarly collapses multiple requests while a paint is in flight and schedules the next invalidation no earlier than the configured frame interval.

`webContents.setFrameRate()` remains a second OSR ceiling. It is not used as the semantic guarantee for JavaScript animation, because Electron documents it as an offscreen-rendering rate control rather than a hard `requestAnimationFrame` contract.

## Explicit rAF ceiling

vosulay registers an Electron session `frame` preload before overlay windows start. Electron runs that preload for frame contexts before normal WebContents preloads, including counter/custom-overlay subframes. The preload installs a Main World `requestAnimationFrame` wrapper before page scripts run.

The wrapper:

- keeps the native rAF as the final scheduler;
- batches pending callbacks;
- sleeps with a timer until just before the configured deadline;
- invokes callbacks no more frequently than the configured FPS;
- preserves `cancelAnimationFrame` semantics for queued callbacks.

This is intentional even when native demand pacing is active. Native pacing controls when vosulay asks Chromium for a new OSR frame; the rAF limiter controls how much animation work untrusted or third-party overlay code is allowed to schedule. Neither is a substitute for the other.

## Why this differs from idk-overlay

idk-overlay provided useful proof points for vosulay: in-game composition, DMA-BUF transport, latest-frame reuse, and producer backpressure are all sound ideas. vosulay does not copy its current Qt pacing loop, however.

At the time this architecture was selected, idk-overlay's Qt webview path used an ACK/request handshake polled by 16 ms single-shot timers (`src/cli/webview/view/poll.cpp`). After a frame is sent, ACK polling is delayed; after the ACK is observed, request polling is delayed again. That makes the webview producer demand-aware, but it also quantizes the control loop to timer polling and can add multiple polling intervals between game consumption and the next producer repaint.

vosulay instead uses these rules:

1. **Presentation is the demand source, not a timer.** Each presenting backend advances demand when the selected game target presents.
2. **The hot path only records demand.** Socket traffic and Node callbacks stay off the game presentation thread.
3. **Demand is coalescible.** Intermediate presents have no resource ownership meaning and can be dropped; only the newest sequence matters.
4. **GPU completion is separate from demand.** `FrameRequest` asks for newer content. `Consumed` is emitted only when native retirement proves Chromium may reclaim a submitted shared texture, including final-use GPU fence completion.
5. **The user FPS setting is an explicit work budget.** It caps Electron invalidation/OSR production and rAF-driven overlay animation even if the game runs at 480, 1000, or more FPS.
6. **Stale content is preferable to game stalls.** If Electron cannot produce a new frame in time, the game reuses the last completed overlay texture. Normal presentation never waits for the renderer.

The result keeps the parts of idk-overlay that matter for latency and bounded ownership while avoiding a timer-mediated ACK round trip as the pacing clock.

## Failure and lifetime invariants

Frame pacing must not weaken the native safety rules:

- GL/Vulkan present hooks remain fail-open and never wait for host IPC.
- A frame request is never interpreted as a release acknowledgement.
- A DMA-BUF is released only after it is retired, no presenter is using it, and all final-use GPU fences have completed.
- If frame-request capability is not negotiated, Electron falls back to fixed-rate OSR at the configured FPS.
- Hiding an overlay stops native demand and OSR painting; showing it resumes on the next selected-target present.
- Disconnect or renderer failure leaves the game presentation path functional; at worst the overlay becomes stale or disappears.

These invariants are more important than producing one unique overlay texture for every game frame. Freshness is bounded by the configured overlay FPS; composition remains synchronized to the game present path.
