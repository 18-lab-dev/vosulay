import assert from "node:assert/strict";
import EventEmitter from "node:events";
import test from "node:test";

import { enableNativeFramePacing, parseFrameRequest } from "../src/main/frame-pacing";
import type { NativeOverlayHandle } from "../src/main/linux-overlay-provider";
import type { ManagedOverlayProcess } from "../src/main/overlay-manager";

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
    return {
        async destroy() {
            this.destroyed = true;
            events.emit("destroyed");
        },
        destroyed: false,
        events,
        async flushPendingSave() {},
        pid: 7,
        async reload() {},
        async setEditing() {},
        async setVisible(value: boolean) {
            this.visibleCalls.push(value);
        },
        visibleCalls: [],
        window: { webContents },
    };
}

test("frame request parsing rejects malformed or non-monotonic primitives", () => {
    assert.deepEqual(parseFrameRequest("7:9"), { sequence: 9, targetId: 7 });
    for (const value of ["", "7", "7:0", "0:1", "x:1", "1:x", "1:2:3", undefined]) {
        assert.equal(parseFrameRequest(value), undefined);
    }
});

test("negotiated pacing coalesces requests and stops after the delivered paint", async () => {
    const webContents = new FakeWebContents(),
        native = new FakeNative(),
        process = processFor(webContents);
    enableNativeFramePacing(process, native as unknown as NativeOverlayHandle);

    assert.equal(webContents.painting, false, "continuous OSR is stopped immediately");
    native.emit("target", "selected:7:1");
    native.emit("frame-request", "7:1");
    native.emit("frame-request", "7:2");
    native.emit("frame-request", "7:2");
    assert.equal(webContents.starts, 1);
    assert.equal(webContents.invalidates, 2, "newer demand invalidates; duplicates do not");

    webContents.emit("paint");
    await new Promise<void>((resolve) => queueMicrotask(resolve));
    assert.equal(webContents.painting, false);

    native.emit("frame-request", "8:3");
    assert.equal(webContents.invalidates, 2, "requests for a non-selected target are ignored");

    await process.setVisible(false);
    native.emit("frame-request", "7:4");
    assert.equal(webContents.invalidates, 2, "hidden overlays do not paint");

    await process.destroy();
    native.emit("frame-request", "7:5");
    assert.equal(webContents.invalidates, 2, "destroy removes native listeners");
});

test("agents without demand capability retain fixed-rate painting", () => {
    const webContents = new FakeWebContents(),
        process = processFor(webContents);
    enableNativeFramePacing(process, {
        supportsFrameRequests: () => false,
    } as NativeOverlayHandle);
    assert.equal(webContents.painting, true);
    assert.equal(webContents.stops, 0);
});
