import assert from "node:assert/strict";
import EventEmitter from "node:events";
import test from "node:test";

import type { ConfigStore } from "../src/main/config";
import type { ForegroundTracker } from "../src/main/foreground-tracker";
import type { LocalOverlayRegistry } from "../src/main/local-overlays";
import { type ManagedOverlayProcess, OverlayManager } from "../src/main/overlay-manager";
import type { RuntimeOptions } from "../src/main/options";

class FakeForegroundTracker implements ForegroundTracker {
    private listener: ((pid: number | undefined) => void) | undefined = undefined;
    stopped = false;

    constructor(private readonly initialPid: number | undefined) {}

    start(onChanged: (pid: number | undefined) => void): number | undefined {
        this.listener = onChanged;
        return this.initialPid;
    }

    emit(pid: number | undefined): void {
        this.listener?.(pid);
    }

    stop(): void {
        this.stopped = true;
        this.listener = undefined;
    }
}

class FakeOverlayProcess implements ManagedOverlayProcess {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    readonly editingValues: boolean[] = [];
    readonly visibilityValues: boolean[] = [];
    readonly window = {
        webContents: {
            setFrameRate: (frameRate: number): void => {
                this.frameRate = frameRate;
            },
        },
    };
    frameRate: number | undefined = undefined;
    destroyed = false;
    destroyStarted = false;
    destroyGate: Promise<void> = Promise.resolve();

    constructor(readonly pid: number) {}

    async destroy(): Promise<void> {
        this.destroyStarted = true;
        await this.destroyGate;
        if (!this.destroyed) {
            this.destroyed = true;
            this.events.emit("destroyed");
        }
    }

    async setEditing(editing: boolean): Promise<void> {
        this.editingValues.push(editing);
    }

    async setVisible(visible: boolean): Promise<void> {
        this.visibilityValues.push(visible);
    }

    async flushPendingSave(): Promise<void> {}
    async reload(): Promise<void> {}
}

const options: RuntimeOptions = {
    fpsLimit: 60,
    pids: [],
    processNames: ["osu!.exe", "osulazer.exe"],
    vosuUrl: new URL("http://127.0.0.1:24050/"),
};

async function eventually(predicate: () => boolean): Promise<void> {
    for (let attempt = 0; attempt < 100; attempt += 1) {
        if (predicate()) {
            return;
        }
        await new Promise<void>((resolve) => setImmediate(resolve));
    }
    assert.fail("condition was not reached");
}

function harness(initialPid: number | undefined, allowed = new Set([11, 22])) {
    const tracker = new FakeForegroundTracker(initialPid),
        initialized: FakeOverlayProcess[] = [];
    let scheduledScan;
    const initializedVisibility: boolean[] = [],
        manager = new OverlayManager(options, {} as ConfigStore, {} as LocalOverlayRegistry, {
            discoverPids: async () => new Set(allowed),
            foregroundTracker: tracker,
            initializeProcess: async (pid, _editingTarget, visible) => {
                initializedVisibility.push(visible);
                const process = new FakeOverlayProcess(pid);
                initialized.push(process);
                return process;
            },
            scheduleScan: (scan) => {
                scheduledScan = scan;
                return () => {
                    scheduledScan = undefined;
                };
            },
        });
    return {
        initialized,
        initializedVisibility,
        manager,
        scan: (): void => {
            assert.ok(scheduledScan);
            scheduledScan();
        },
        tracker,
    };
}

test("selects exactly the allowed foreground PID", async () => {
    const state = harness(22);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    assert.deepEqual(
        state.initialized.map(({ pid }) => pid),
        [22],
    );
    assert.equal(state.initialized[0]?.frameRate, 60);
    await state.manager.stop();
    assert.equal(state.tracker.stopped, true);
});

test("attaches native process without a BrowserWindow", async () => {
    const tracker = new FakeForegroundTracker(11);
    let attached = false;
    const manager = new OverlayManager(options, {} as ConfigStore, {} as LocalOverlayRegistry, {
        discoverPids: async () => new Set([11]),
        foregroundTracker: tracker,
        initializeProcess: async (pid) => {
            const process: ManagedOverlayProcess = {
                destroy: async () => {},
                events: new EventEmitter<{ destroyed: [] }>(),
                flushPendingSave: async () => {},
                pid,
                reload: async () => {},
                setEditing: async () => {},
                setVisible: async () => {
                    attached = true;
                },
            };
            return process;
        },
        scheduleScan: () => () => {},
    });

    manager.start();
    await eventually(() => attached);
    await manager.stop();
});

test("destroys stable before attaching lazer", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    const stable = state.initialized[0]!;
    let releaseDestroy!: () => void;
    stable.destroyGate = new Promise<void>((resolve) => {
        releaseDestroy = resolve;
    });

    state.tracker.emit(22);
    await eventually(() => stable.destroyStarted);
    assert.equal(state.initialized.length, 1);
    releaseDestroy();
    await eventually(() => state.initialized.length === 2);
    assert.equal(state.initialized[1]?.pid, 22);
    await state.manager.stop();
});

test("keeps the last focused game attached until another game receives focus", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    const stable = state.initialized[0]!;

    state.tracker.emit(999);
    await new Promise<void>((resolve) => setImmediate(resolve));
    state.scan();
    await new Promise<void>((resolve) => setImmediate(resolve));
    assert.equal(stable.destroyed, false);
    assert.equal(state.initialized.length, 1);

    state.tracker.emit(22);
    state.tracker.emit(999);
    await eventually(() => stable.destroyed && state.initialized.length === 2);
    assert.equal(state.initialized[1]?.pid, 22);
    await state.manager.stop();
});

test("disposes a stale attach before selecting the latest foreground PID", async () => {
    const tracker = new FakeForegroundTracker(11),
        initialized: FakeOverlayProcess[] = [];
    let resolveStable!: (process: FakeOverlayProcess) => void;
    const stableInitialization = new Promise<FakeOverlayProcess>((resolve) => {
            resolveStable = resolve;
        }),
        manager = new OverlayManager(options, {} as ConfigStore, {} as LocalOverlayRegistry, {
            discoverPids: async () => new Set([11, 22]),
            foregroundTracker: tracker,
            initializeProcess: async (pid) => {
                if (pid === 11) {
                    const process = await stableInitialization;
                    initialized.push(process);
                    return process;
                }
                const process = new FakeOverlayProcess(pid);
                initialized.push(process);
                return process;
            },
            scheduleScan: () => () => {},
        });

    manager.start();
    await new Promise<void>((resolve) => setImmediate(resolve));
    const editingUpdate = manager.setEditing(true);
    tracker.emit(22);
    const stale = new FakeOverlayProcess(11);
    resolveStable(stale);
    await eventually(() => stale.destroyed && initialized.some(({ pid }) => pid === 22));
    await editingUpdate;
    assert.equal(stale.editingValues.includes(true), false);
    assert.deepEqual(
        initialized.map(({ pid }) => pid),
        [11, 22],
    );
    await manager.stop();
});

test("retries a failed attach on the next process scan", async () => {
    const tracker = new FakeForegroundTracker(11);
    let attached,
        attempts = 0,
        scan;
    const manager = new OverlayManager(options, {} as ConfigStore, {} as LocalOverlayRegistry, {
        discoverPids: async () => new Set([11]),
        foregroundTracker: tracker,
        initializeProcess: async (pid) => {
            attempts += 1;
            if (attempts === 1) {
                throw new Error("attach failed");
            }
            attached = new FakeOverlayProcess(pid);
            return attached;
        },
        scheduleScan: (callback) => {
            scan = callback;
            return () => {
                scan = undefined;
            };
        },
    });

    manager.start();
    await eventually(() => attempts === 1);
    assert.ok(scan);
    scan();
    await eventually(() => attached !== undefined);
    assert.equal(attempts, 2);
    await manager.stop();
});

test("reattaches the last focused game when the active overlay reports destruction", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    state.tracker.emit(999);
    await new Promise<void>((resolve) => setImmediate(resolve));
    state.initialized[0]!.events.emit("destroyed");
    await eventually(() => state.initialized.length === 2);
    assert.deepEqual(
        state.initialized.map(({ pid }) => pid),
        [11, 11],
    );
    await state.manager.stop();
});

test("propagates edit mode to the active and replacement process", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    await state.manager.setEditing(true);
    assert.equal(state.initialized[0]?.editingValues.at(-1), true);

    state.tracker.emit(22);
    await eventually(() => state.initialized.length === 2);
    assert.equal(state.initialized[1]?.editingValues.at(-1), true);
    await state.manager.stop();
});

test("hides the active overlay and keeps replacements hidden", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    await state.manager.setEditing(true);
    await state.manager.toggleVisibility();
    assert.equal(state.manager.visible, false);
    assert.equal(state.initialized[0]?.visibilityValues.at(-1), false);
    assert.equal(state.initialized[0]?.editingValues.at(-1), false);

    state.tracker.emit(22);
    await eventually(() => state.initialized.length === 2);
    assert.equal(state.initializedVisibility.at(-1), false);
    assert.equal(state.initialized[1]?.visibilityValues.at(-1), false);
    await state.manager.stop();
});

test("preserves a visibility toggle received before startup", async () => {
    const state = harness(11);
    await state.manager.toggleVisibility();
    assert.equal(state.manager.visible, false);

    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    assert.equal(state.initializedVisibility.at(-1), false);
    assert.equal(state.initialized[0]?.visibilityValues.at(-1), false);
    await state.manager.stop();
});
