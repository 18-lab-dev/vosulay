import assert from "node:assert/strict";
import test from "node:test";

import {
    bitmapFrame,
    cdpInput,
    offscreenPreference,
    electronInput,
    EMPTY_TARGET_STATE,
    nextTargetState,
    parseInput,
    parseTargetEvent,
    StartupTransaction,
} from "../src/main/linux-overlay-process";
import { NativeSharedTexture } from "../src/main/linux-overlay-provider";

function textureInfo(
    pixelFormat: "rgba" | "bgra" | "rgbaf16" = "bgra",
    planes: Electron.Planes[] = [{ fd: 42, stride: 2560, offset: 0, size: 1_228_800 }],
): Electron.TextureInfo {
    return {
        widgetType: "frame",
        pixelFormat,
        codedSize: { width: 640, height: 480 },
        colorSpace: {} as Electron.ColorSpace,
        visibleRect: { x: 0, y: 0, width: 640, height: 480 },
        contentRect: { x: 0, y: 0, width: 640, height: 480 },
        timestamp: 0,
        metadata: {},
        handle: {
            nativePixmap: {
                planes,
                modifier: "72057594037927935",
                supportsZeroCopyWebGpuImport: false,
            },
        },
    };
}

test("maps Electron BGRA DMA-BUF metadata without copying plane descriptors", () => {
    assert.deepEqual(
        NativeSharedTexture.fromElectron(
            9,
            17,
            textureInfo(),
            { x: 1, y: 2, width: 3, height: 4 },
        ),
        {
            targetId: 9,
            sequence: 17,
            width: 640,
            height: 480,
            fourcc: "AR24",
            modifier: "72057594037927935",
            planes: [{ fd: 42, stride: 2560, offset: 0, size: 1_228_800 }],
        },
    );
});


test("targets that cannot sample imported buffers receive CPU frames", () => {
    assert.deepEqual(offscreenPreference({ supportsDmabuf: () => true }), {
        useSharedTexture: true,
        sharedTexturePixelFormat: "argb",
    });
    assert.equal(offscreenPreference({ supportsDmabuf: () => false }), true);
    assert.deepEqual(offscreenPreference({}), {
        useSharedTexture: true,
        sharedTexturePixelFormat: "argb",
    });
});

test("CPU frames are forwarded with tightly packed BGRA geometry", () => {
    const bitmap = Buffer.alloc(8 * 4 * 4);
    assert.deepEqual(
        bitmapFrame(7, { getSize: () => ({ width: 8, height: 4 }), toBitmap: () => bitmap }),
        { descriptor: { targetId: 7, width: 8, height: 4, stride: 32 }, bitmap },
    );
    assert.equal(
        bitmapFrame(7, { getSize: () => ({ width: 0, height: 4 }), toBitmap: () => bitmap }),
        null,
    );
    assert.equal(
        bitmapFrame(7, {
            getSize: () => ({ width: 8, height: 4 }),
            toBitmap: () => Buffer.alloc(16),
        }),
        null,
    );
});
test("maps Electron RGBA and rejects unsupported shared textures", () => {
    assert.equal(NativeSharedTexture.fromElectron(
        1,
        1,
        textureInfo("rgba"),
        { x: 0, y: 0, width: 640, height: 480 },
    )?.fourcc, "AB24");
    assert.equal(NativeSharedTexture.fromElectron(
        1,
        1,
        textureInfo("rgbaf16"),
        { x: 0, y: 0, width: 640, height: 480 },
    ), null);
    assert.equal(NativeSharedTexture.fromElectron(
        1,
        1,
        textureInfo("bgra", []),
        { x: 0, y: 0, width: 640, height: 480 },
    ), null);
});

test("creating a target registers it without making it active", () => {
    const created = parseTargetEvent("created:9:640:480");
    assert.deepEqual(created, {
        kind: "created",
        target: { id: 9, width: 640, height: 480 },
    });
    // Creation order used to imply selection, which is what let Electron and
    // the agent disagree about where frames should go.
    const state = nextTargetState(EMPTY_TARGET_STATE, created!);
    assert.equal(state.active, null);
    assert.deepEqual(state.known.get(9), { id: 9, width: 640, height: 480 });
});

test("the agent's selection decides the active target", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("created:11:1920:1080")!);
    // The most recently created target is not automatically the winner.
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    assert.deepEqual(state.active, { id: 9, width: 640, height: 480 });
    assert.equal(state.generation, 1);

    state = nextTargetState(state, parseTargetEvent("selected:11:2")!);
    assert.deepEqual(state.active, { id: 11, width: 1920, height: 1080 });
});

test("a stale selection never undoes a newer one", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("created:11:1920:1080")!);
    state = nextTargetState(state, parseTargetEvent("selected:11:5")!);
    const reordered = nextTargetState(state, parseTargetEvent("selected:9:4")!);
    assert.equal(reordered, state, "an older generation must be ignored outright");
    assert.deepEqual(reordered.active, { id: 11, width: 1920, height: 1080 });
});

test("selecting target zero clears the active target", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    state = nextTargetState(state, parseTargetEvent("selected:0:2")!);
    assert.equal(state.active, null);
    assert.equal(state.generation, 2);
});

test("destroying the active target stops forwarding", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);

    const unrelated = nextTargetState(state, parseTargetEvent("destroyed:8")!);
    assert.deepEqual(unrelated.active, { id: 9, width: 640, height: 480 });

    const destroyed = nextTargetState(state, parseTargetEvent("destroyed:9")!);
    assert.equal(destroyed.active, null);
    assert.equal(destroyed.known.has(9), false);
});

test("resizing the active target is reflected immediately", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    state = nextTargetState(state, parseTargetEvent("created:9:1280:720")!);
    assert.deepEqual(state.active, { id: 9, width: 1280, height: 720 });
});

test("a selection naming an unknown target clears rather than guesses", () => {
    const state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("selected:42:1")!);
    assert.equal(state.active, null);
    assert.equal(state.generation, 1, "the decision is still newer than what came before");
});

test("rejects malformed target events", () => {
    assert.equal(parseTargetEvent("destroyed:0"), null);
    assert.equal(parseTargetEvent("destroyed:9:extra"), null);
    assert.equal(parseTargetEvent("selected:9"), null);
    assert.equal(parseTargetEvent("selected:9:0"), null, "generations start at one");
    assert.equal(parseTargetEvent("selected:-1:1"), null);
    assert.equal(parseTargetEvent("selected:9:x"), null);
    assert.equal(parseTargetEvent("selected:9:1:2"), null);
    // Zero is a valid selection only, never a valid target id.
    assert.deepEqual(parseTargetEvent("selected:0:3"), {
        kind: "selected",
        targetId: 0,
        generation: 3,
    });
});

test("a failed createSurface releases the window and the native handle", async () => {
    // The window is constructed before the surface exists, so a rejection here
    // used to leak both: no LinuxOverlayProcess existed yet whose destroy()
    // could have cleaned them up.
    const released: string[] = [];
    let windowDestroyed = false;
    const startup = new StartupTransaction();

    let failure: unknown;
    try {
        // Same acquisition order as LinuxOverlayProcess.create().
        startup.acquire(() => {
            released.push("native");
        });
        startup.acquire(() => {
            if (!windowDestroyed) {
                windowDestroyed = true;
                released.push("window");
            }
        });
        await Promise.reject(new Error("no DRM device"));
    } catch (error) {
        failure = error;
        await startup.unwind();
    }

    assert.ok(failure instanceof Error);
    assert.equal(failure.message, "no DRM device");
    assert.deepEqual(released, ["window", "native"], "reverse acquisition order");
    assert.equal(windowDestroyed, true);
});

test("a committed startup transaction releases nothing", async () => {
    // Once the process instance exists it owns the resources, so unwinding
    // must not destroy a live overlay.
    const released: string[] = [];
    const startup = new StartupTransaction();
    startup.acquire(() => released.push("native"));
    startup.acquire(() => released.push("window"));
    startup.commit();
    await startup.unwind();
    assert.deepEqual(released, []);
});

test("a failing rollback does not prevent the others from running", async () => {
    const released: string[] = [];
    const startup = new StartupTransaction();
    startup.acquire(() => released.push("native"));
    startup.acquire(() => {
        throw new Error("window teardown failed");
    });
    await startup.unwind();
    assert.deepEqual(released, ["native"], "an earlier resource is still released");
});

test("validates and translates native SDL input events", () => {
    const mouse = parseInput("9:4:0:0:123:456");
    assert.deepEqual(mouse, {
        targetId: 9,
        kind: 4,
        code: 0,
        flags: 0,
        value: 123,
        value2: 456,
    });
    assert.deepEqual(electronInput(mouse!), {
        type: "mouseMove",
        x: 123,
        y: 456,
    });
    assert.deepEqual(electronInput(parseInput("9:5:1:2:123:456")!), {
        type: "mouseDown",
        x: 123,
        y: 456,
        button: "left",
        clickCount: 2,
    });
    assert.deepEqual(electronInput(parseInput("9:7:0:0:-120:240")!), {
        type: "mouseWheel",
        x: 0,
        y: 0,
        deltaX: -120,
        deltaY: 240,
    });
    assert.deepEqual(electronInput(parseInput("9:1:4:2147483648:97:0")!), {
        type: "keyDown",
        keyCode: "a",
        modifiers: ["isautorepeat"],
    });
    assert.deepEqual(electronInput(parseInput("9:3:0:0:955:0")!), {
        type: "char",
        keyCode: "λ",
    });
});

test("rejects malformed or unsupported native input", () => {
    assert.equal(parseInput("9:4:0"), null);
    assert.equal(parseInput("9:4:0:0:NaN:1"), null);
    assert.equal(electronInput(parseInput("9:5:9:1:1:1")!), null);
    assert.equal(electronInput(parseInput("9:3:0:0:55296:0")!), null);
    assert.equal(electronInput(parseInput("9:99:0:0:0:0")!), null);
});

test("maps native input to focus-independent CDP commands", () => {
    assert.deepEqual(cdpInput(parseInput("9:4:0:0:123:456")!), {
        method: "Input.dispatchMouseEvent",
        params: { type: "mouseMoved", x: 123, y: 456, modifiers: 0 },
    });
    assert.deepEqual(cdpInput(parseInput("9:5:1:2:123:456")!), {
        method: "Input.dispatchMouseEvent",
        params: {
            type: "mousePressed",
            x: 123,
            y: 456,
            button: "left",
            clickCount: 2,
            modifiers: 8,
        },
    });
    assert.deepEqual(cdpInput(parseInput("9:3:0:0:955:0")!), {
        method: "Input.insertText",
        params: { text: "λ" },
    });
    assert.deepEqual(cdpInput(parseInput("9:1:4:2147483712:97:0")!), {
        method: "Input.dispatchKeyEvent",
        params: {
            type: "keyDown",
            key: "a",
            code: "a",
            modifiers: 2,
            autoRepeat: true,
        },
    });
});
