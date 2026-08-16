import { type ManagedOverlayProcess, OverlayManager } from "../src/main/overlay-manager";
import type { ConfigStore } from "../src/main/config";
import EventEmitter from "node:events";
import type { ForegroundTracker } from "../src/main/foreground-tracker";
import type { LocalOverlayRegistry } from "../src/main/local-overlays";
import { type RuntimeOptions } from "../src/main/options";
import assert from "node:assert/strict";
import test from "node:test";

const options: RuntimeOptions = {
    fpsLimit: 60,
    pids: [],
    processNames: ["osu!.exe", "osulazer.exe"],
    vosuUrl: new URL("http://127.0.0.1:24050/"),
};

function createConfigStoreStub(): ConfigStore {
    /* SAFETY: These tests only require constructor injection; no configuration read or write occurs on this stub. */
    return {} as ConfigStore;
}

function createLocalOverlayRegistryStub(): LocalOverlayRegistry {
    /* SAFETY: These tests only exercise overlay lifecycle behavior and do not call registry APIs. */
    return {} as LocalOverlayRegistry;
}

function nextTick(): Promise<void> {
    return new Promise((resolve) => {
        void setImmediate(resolve);
    });
}

function eventually(predicate: () => boolean): Promise<void> {
    const waitForCondition = (remaining: number): Promise<void> => {
        if (predicate()) {
            return Promise.resolve();
        }
        if (remaining <= 0) {
            assert.fail("condition was not reached");
        }
        return nextTick().then(() => waitForCondition(remaining - 1));
    };
    return waitForCondition(100);
}

class FakeForegroundTracker implements ForegroundTracker {
    private listener: ((pid: number | undefined) => void) | undefined = undefined;
    private readonly initialPid: number | undefined;
    stopped = false;

    constructor(initialPid: number | undefined) {
        this.initialPid = initialPid;
    }

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
    readonly pid: number;
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

    constructor(pid: number) {
        this.pid = pid;
    }

    async destroy(): Promise<void> {
        this.destroyStarted = true;
        await this.destroyGate;
        if (!this.destroyed) {
            this.destroyed = true;
            this.events.emit("destroyed");
        }
    }

    setEditing(editing: boolean): Promise<void> {
        this.editingValues.push(editing);
        return Promise.resolve();
    }

    setVisible(visible: boolean): Promise<void> {
        this.visibilityValues.push(visible);
        return Promise.resolve();
    }

    flushPendingSave(): Promise<void> {
        return Promise.resolve();
    }

    reload(): Promise<void> {
        return Promise.resolve();
    }
}
const failUninitializedScan = (): void => {
    assert.fail("scan callback was not registered");
};
const failCancelledScan = (): void => {
    assert.fail("scan callback was already cancelled");
};
const failUninitializedRelease = (): void => {
    assert.fail("release callback was not initialized");
};
const failUninitializedResolveStable = (_process: FakeOverlayProcess): void => {
    assert.fail("resolution callback was not initialized");
};
function harness(initialPid: number | undefined, allowed = new Set([11, 22])) {
    const tracker = new FakeForegroundTracker(initialPid);
    const initialized: FakeOverlayProcess[] = [];
    let scheduledScan: () => void = failUninitializedScan;
    const initializedVisibility: boolean[] = [];
    const manager = new OverlayManager(
        options,
        createConfigStoreStub(),
        createLocalOverlayRegistryStub(),
        {
            discoverPids: () => Promise.resolve(new Set(allowed)),
            foregroundTracker: tracker,
            initializeProcess: (pid, _editingTarget, visible) => {
                initializedVisibility.push(visible);
                const process = new FakeOverlayProcess(pid);
                initialized.push(process);
                return Promise.resolve(process);
            },
            scheduleScan: (scan) => {
                scheduledScan = scan;
                return () => {
                    scheduledScan = failCancelledScan;
                };
            },
        },
    );
    return {
        initialized,
        initializedVisibility,
        manager,
        scan: () => {
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
    const manager = new OverlayManager(
        options,
        createConfigStoreStub(),
        createLocalOverlayRegistryStub(),
        {
            discoverPids: () => Promise.resolve(new Set([11])),
            foregroundTracker: tracker,
            initializeProcess: (pid) => {
                const process: ManagedOverlayProcess = {
                    destroy: () => Promise.resolve(),
                    events: new EventEmitter<{ destroyed: [] }>(),
                    flushPendingSave: () => Promise.resolve(),
                    pid,
                    reload: () => Promise.resolve(),
                    setEditing: () => Promise.resolve(),
                    setVisible: () => {
                        attached = true;
                        return Promise.resolve();
                    },
                };
                return Promise.resolve(process);
            },
            scheduleScan: () => () => {},
        },
    );

    manager.start();
    await eventually(() => attached);
    await manager.stop();
});
test("destroys stable before attaching lazer", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    const stable = state.initialized[0]!;
    let releaseDestroy: () => void = failUninitializedRelease;
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
    await nextTick();
    state.scan();
    await nextTick();
    assert.equal(stable.destroyed, false);
    assert.equal(state.initialized.length, 1);

    state.tracker.emit(22);
    state.tracker.emit(999);
    await eventually(() => stable.destroyed && state.initialized.length === 2);
    assert.equal(state.initialized[1]?.pid, 22);
    await state.manager.stop();
});

test("disposes a stale attach before selecting the latest foreground PID", async () => {
    const tracker = new FakeForegroundTracker(11);
    const initialized: FakeOverlayProcess[] = [];
    let resolveStable: (process: FakeOverlayProcess) => void = failUninitializedResolveStable;
    const stableInitialization = new Promise<FakeOverlayProcess>((resolve) => {
        resolveStable = resolve;
    });
    const manager = new OverlayManager(
        options,
        createConfigStoreStub(),
        createLocalOverlayRegistryStub(),
        {
            discoverPids: () => Promise.resolve(new Set([11, 22])),
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
        },
    );

    manager.start();
    await nextTick();
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
    const attached: FakeOverlayProcess[] = [];
    let attempts = 0;
    let scan: () => void = failUninitializedScan;
    const manager = new OverlayManager(
        options,
        createConfigStoreStub(),
        createLocalOverlayRegistryStub(),
        {
            discoverPids: () => Promise.resolve(new Set([11])),
            foregroundTracker: tracker,
            initializeProcess: (pid) => {
                attempts += 1;
                if (attempts === 1) {
                    throw new Error("attach failed");
                }
                const process = new FakeOverlayProcess(pid);
                attached.push(process);
                return Promise.resolve(process);
            },
            scheduleScan: (callback) => {
                scan = callback;
                return () => {
                    scan = failCancelledScan;
                };
            },
        },
    );

    manager.start();
    await eventually(() => attempts === 1);
    scan();
    await eventually(() => attached.length === 1);
    assert.equal(attempts, 2);
    await manager.stop();
});

test("reattaches the last focused game when the active overlay reports destruction", async () => {
    const state = harness(11);
    state.manager.start();
    await eventually(() => state.initialized.length === 1);
    state.tracker.emit(999);
    await new Promise<void>((resolve) => {
        void setImmediate(resolve);
    });
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
