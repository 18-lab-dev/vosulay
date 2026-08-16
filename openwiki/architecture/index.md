---
type: Documentation Index
title: Architecture
description: Architecture documentation for the shared Electron host and platform in-game overlay providers.
---

# Architecture

- [vosulay architecture overview](overview.md) - Explains the canonical Electron host, shared services, Windows asdf provider, Linux authenticated native stack, offscreen frame ownership, input, packaging, and shutdown invariants.
- [Frame pacing](frame-pacing.md) - Explains game-present-driven Linux demand, configured OSR/rAF ceilings, GPU lifetime separation, and why vosulay does not use idk-overlay's timer-polled ACK loop as its pacing clock.
