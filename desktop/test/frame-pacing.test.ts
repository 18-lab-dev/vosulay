import {
    enableNativeFramePacing,
    frameRequestDelayMs,
    parseFrameRequest,
} from "../src/main/frame-pacing";
import EventEmitter from "node:events";
import type { ManagedOverlayProcess } from "../src/main/overlay-manager";
import type { NativeOverlayHandle } from "../src/main/linux-overlay-provider";
import assert from "node:assert/strict";
import test from "node:test";

class FakeWebContents extends EventEmitter {
    painting = true;
    starts = 0;
    stops = 0;
    invalidates = 0;

    isDestroyed(): boolean {
        return false;
    }
    isPainting(): boolean {
        return this.painting;
    }
    startPainting(): void {
        this.painting = true;
        this.starts += 1;
    }
    stopPainting(): void {
        this.painting = false;
        this.stops += 1;
    }
    invalidate(): void {
        this.invalidates += 1;
    }
}

class FakeNative extends EventEmitter {
    supportsFrameRequests(): boolean {
        return true;
    }
}

function processFor(webContents: FakeWebContents): ManagedOverlayProcess & {
    window: { webContents: FakeWebContents };
    visibleCalls: boolean[];
    destroyed: boolean;
} {
    const events = new EventEmitter<{ destroyed: [] }>();
    const visibleCalls: boolean[] = [];
    const process: ManagedOverlayProcess & {
        window: { webContents: FakeWebContents };
        visibleCalls: boolean[];
        destroyed: boolean;
        destroy: () => Promise<void>;
        flushPendingSave: () => Promise<void>;
        reload: () => Promise<void>;
        setEditing: () => Promise<void>;
        setVisible: (visible: boolean) => Promise<void>;
    } = {
        destroy: () => {
            process.destroyed = true;
            events.emit("destroyed");
            return Promise.resolve();
        },
        destroyed: false,
        events,
        flushPendingSave: () => Promise.resolve(),
        pid: 7,
        reload: () => Promise.resolve(),
        setEditing: () => Promise.resolve(),
        setVisible: (value: boolean) => {
            visibleCalls.push(value);
            return Promise.resolve();
        },
        visibleCalls,
        window: { webContents },
    };
    return process;
}

test("frame request parsing rejects malformed or non-monotonic primitives", () => {
    assert.deepEqual(parseFrameRequest("7:9"), { sequence: 9, targetId: 7 });
    for (const value of ["", "7", "7:0", "0:1", "x:1", "1:x", "1:2:3", undefined]) {
        assert.equal(parseFrameRequest(value), undefined);
    }
});

test("configured frame pacing delay enforces the requested ceiling", () => {
    assert.equal(frameRequestDelayMs(100, 101, 250), 3);
    assert.equal(frameRequestDelayMs(100, 104, 250), 0);
    assert.equal(frameRequestDelayMs(Number.NEGATIVE_INFINITY, 100, 250), 0);
});

test("negotiated pacing coalesces requests and stops after the delivered paint", async () => {
    const webContents = new FakeWebContents();
    const nativeBus = new FakeNative();
    const native: NativeOverlayHandle = {
        destroy: () => Promise.resolve(),
        off: (event: string, callback: (payload: string) => void) => {
            nativeBus.off(event, callback);
        },
        on: (event: string, callback: (payload: string) => void) => {
            nativeBus.on(event, callback);
        },
        supportsFrameRequests: () => nativeBus.supportsFrameRequests(),
    };
    const process = processFor(webContents);
    enableNativeFramePacing(process, native, 240);

    assert.equal(webContents.painting, false, "continuous OSR is stopped immediately");
    nativeBus.emit("target", "selected:7:1");
    nativeBus.emit("frame-request", "7:1");
    nativeBus.emit("frame-request", "7:2");
    nativeBus.emit("frame-request", "7:2");
    assert.equal(webContents.starts, 1);
    assert.equal(webContents.invalidates, 1, "newer demand coalesces into the in-flight paint");

    webContents.emit("paint");
    await Promise.resolve();
    assert.equal(webContents.painting, false);

    nativeBus.emit("frame-request", "8:3");
    assert.equal(webContents.invalidates, 1, "requests for a non-selected target are ignored");

    await process.setVisible(false);
    nativeBus.emit("frame-request", "7:4");
    assert.equal(webContents.invalidates, 1, "hidden overlays do not paint");

    await process.destroy();
    nativeBus.emit("frame-request", "7:5");
    assert.equal(webContents.invalidates, 1, "destroy removes native listeners");
});

test("agents without demand capability retain fixed-rate painting", () => {
    const webContents = new FakeWebContents();
    const process = processFor(webContents);
    enableNativeFramePacing(
        process,
        {
            destroy: () => Promise.resolve(),
            supportsFrameRequests: () => false,
        },
        60,
    );
    assert.equal(webContents.painting, true);
    assert.equal(webContents.stops, 0);
});
