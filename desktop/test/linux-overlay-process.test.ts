import {
    EMPTY_TARGET_STATE,
    StartupTransaction,
    bitmapFrame,
    cdpInput,
    electronInput,
    nextTargetState,
    offscreenPreference,
    parseInput,
    parseTargetEvent,
} from "../src/main/linux-overlay-process";
import { NativeSharedTexture } from "../src/main/linux-overlay-provider";
import assert from "node:assert/strict";
import test from "node:test";

function textureInfo(
    pixelFormat: "rgba" | "bgra" | "rgbaf16" = "bgra",
    planes: Electron.Planes[] = [{ fd: 42, offset: 0, size: 1_228_800, stride: 2560 }],
): Electron.TextureInfo {
    return {
        codedSize: { height: 480, width: 640 },
        /* SAFETY: The texture fixture intentionally mirrors Electron's shape and only exercises test plumbing. */
        colorSpace: {} as Electron.ColorSpace,
        contentRect: { height: 480, width: 640, x: 0, y: 0 },
        handle: {
            nativePixmap: {
                modifier: "72057594037927935",
                planes,
                supportsZeroCopyWebGpuImport: false,
            },
        },
        metadata: {},
        pixelFormat,
        timestamp: 0,
        visibleRect: { height: 480, width: 640, x: 0, y: 0 },
        widgetType: "frame",
    };
}

test("maps Electron BGRA DMA-BUF metadata without copying plane descriptors", () => {
    assert.deepEqual(
        NativeSharedTexture.fromElectron(9, 17, textureInfo(), { height: 4, width: 3, x: 1, y: 2 }),
        {
            fourcc: "AR24",
            height: 480,
            modifier: "72057594037927935",
            planes: [{ fd: 42, offset: 0, size: 1_228_800, stride: 2560 }],
            sequence: 17,
            targetId: 9,
            width: 640,
        },
    );
});

test("targets that cannot sample imported buffers receive CPU frames", () => {
    assert.deepEqual(offscreenPreference({ supportsDmabuf: () => true }), {
        sharedTexturePixelFormat: "argb",
        useSharedTexture: true,
    });
    assert.equal(offscreenPreference({ supportsDmabuf: () => false }), true);
    assert.deepEqual(offscreenPreference({}), {
        sharedTexturePixelFormat: "argb",
        useSharedTexture: true,
    });
});

test("CPU frames are forwarded with tightly packed BGRA geometry", () => {
    const bitmap = Buffer.alloc(8 * 4 * 4);
    assert.deepEqual(
        bitmapFrame(7, { getSize: () => ({ height: 4, width: 8 }), toBitmap: () => bitmap }),
        { bitmap, descriptor: { height: 4, stride: 32, targetId: 7, width: 8 } },
    );
    assert.equal(
        bitmapFrame(7, { getSize: () => ({ height: 4, width: 0 }), toBitmap: () => bitmap }),
        undefined,
    );
    assert.equal(
        bitmapFrame(7, {
            getSize: () => ({ height: 4, width: 8 }),
            toBitmap: () => Buffer.alloc(16),
        }),
        undefined,
    );
});

test("maps Electron RGBA and rejects unsupported shared textures", () => {
    assert.equal(
        NativeSharedTexture.fromElectron(1, 1, textureInfo("rgba"), {
            height: 480,
            width: 640,
            x: 0,
            y: 0,
        })?.fourcc,
        "AB24",
    );
    assert.equal(
        NativeSharedTexture.fromElectron(1, 1, textureInfo("rgbaf16"), {
            height: 480,
            width: 640,
            x: 0,
            y: 0,
        }),
        undefined,
    );
    assert.equal(
        NativeSharedTexture.fromElectron(1, 1, textureInfo("bgra", []), {
            height: 480,
            width: 640,
            x: 0,
            y: 0,
        }),
        undefined,
    );
});

test("creating a target registers it without making it active", () => {
    const created = parseTargetEvent("created:9:640:480");
    assert.deepEqual(created, {
        kind: "created",
        target: { height: 480, id: 9, width: 640 },
    });
    const state = nextTargetState(EMPTY_TARGET_STATE, created!);
    assert.equal(state.active, undefined);
    assert.deepEqual(state.known.get(9), { height: 480, id: 9, width: 640 });
});

test("the agent's selection decides the active target", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("created:11:1920:1080")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    assert.deepEqual(state.active, { height: 480, id: 9, width: 640 });
    assert.equal(state.generation, 1);

    state = nextTargetState(state, parseTargetEvent("selected:11:2")!);
    assert.deepEqual(state.active, { height: 1080, id: 11, width: 1920 });
});

test("a stale selection never undoes a newer one", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("created:11:1920:1080")!);
    state = nextTargetState(state, parseTargetEvent("selected:11:5")!);
    const reordered = nextTargetState(state, parseTargetEvent("selected:9:4")!);
    assert.equal(reordered, state, "an older generation must be ignored outright");
    assert.deepEqual(reordered.active, { height: 1080, id: 11, width: 1920 });
});

test("selecting target zero clears the active target", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    state = nextTargetState(state, parseTargetEvent("selected:0:2")!);
    assert.equal(state.active, undefined);
    assert.equal(state.generation, 2);
});

test("destroying the active target stops forwarding", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);

    const unrelated = nextTargetState(state, parseTargetEvent("destroyed:8")!);
    assert.deepEqual(unrelated.active, { height: 480, id: 9, width: 640 });

    const destroyed = nextTargetState(state, parseTargetEvent("destroyed:9")!);
    assert.equal(destroyed.active, undefined);
    assert.equal(destroyed.known.has(9), false);
});

test("resizing the active target is reflected immediately", () => {
    let state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("created:9:640:480")!);
    state = nextTargetState(state, parseTargetEvent("selected:9:1")!);
    state = nextTargetState(state, parseTargetEvent("created:9:1280:720")!);
    assert.deepEqual(state.active, { height: 720, id: 9, width: 1280 });
});

test("a selection naming an unknown target clears rather than guesses", () => {
    const state = nextTargetState(EMPTY_TARGET_STATE, parseTargetEvent("selected:42:1")!);
    assert.equal(state.active, undefined);
    assert.equal(state.generation, 1, "the decision is still newer than what came before");
});

test("rejects malformed target events", () => {
    assert.equal(parseTargetEvent("destroyed:0"), undefined);
    assert.equal(parseTargetEvent("destroyed:9:extra"), undefined);
    assert.equal(parseTargetEvent("selected:9"), undefined);
    assert.equal(parseTargetEvent("selected:9:0"), undefined, "generations start at one");
    assert.equal(parseTargetEvent("selected:-1:1"), undefined);
    assert.equal(parseTargetEvent("selected:9:x"), undefined);
    assert.equal(parseTargetEvent("selected:9:1:2"), undefined);
    assert.deepEqual(parseTargetEvent("selected:0:3"), {
        generation: 3,
        kind: "selected",
        targetId: 0,
    });
});

test("a failed createSurface releases the window and the native handle", async () => {
    const released: string[] = [];
    let windowDestroyed = false;
    const startup = new StartupTransaction();

    let failure: Error | undefined = new Error("expected startup failure");
    try {
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
        if (!(error instanceof Error)) {
            throw error;
        }
        failure = error;
        await startup.unwind();
    }

    assert.ok(failure instanceof Error);
    assert.equal(failure.message, "no DRM device");
    assert.deepEqual(released, ["window", "native"], "reverse acquisition order");
    assert.equal(windowDestroyed, true);
});

test("a committed startup transaction releases nothing", async () => {
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
        code: 0,
        flags: 0,
        kind: 4,
        targetId: 9,
        value: 123,
        value2: 456,
    });
    assert.deepEqual(electronInput(mouse!), {
        type: "mouseMove",
        x: 123,
        y: 456,
    });
    assert.deepEqual(electronInput(parseInput("9:5:1:2:123:456")!), {
        button: "left",
        clickCount: 2,
        type: "mouseDown",
        x: 123,
        y: 456,
    });
    assert.deepEqual(electronInput(parseInput("9:7:0:0:-120:240")!, { x: 123, y: 456 }), {
        deltaX: -120,
        deltaY: 240,
        type: "mouseWheel",
        x: 123,
        y: 456,
    });
    assert.deepEqual(electronInput(parseInput("9:1:4:2147483648:97:0")!), {
        keyCode: "a",
        modifiers: ["isautorepeat"],
        type: "keyDown",
    });
    assert.deepEqual(electronInput(parseInput("9:3:0:0:955:0")!), {
        keyCode: "λ",
        type: "char",
    });
});

test("rejects malformed or unsupported native input", () => {
    assert.equal(parseInput("9:4:0"), undefined);
    assert.equal(parseInput("9:4:0:0:NaN:1"), undefined);
    assert.equal(electronInput(parseInput("9:5:9:1:1:1")!), undefined);
    assert.equal(electronInput(parseInput("9:3:0:0:55296:0")!), undefined);
    assert.equal(electronInput(parseInput("9:99:0:0:0:0")!), undefined);
});

test("click counts never become keyboard modifiers in CDP translations", () => {
    assert.deepEqual(cdpInput(parseInput("9:4:0:0:123:456")!), {
        method: "Input.dispatchMouseEvent",
        params: { modifiers: 0, type: "mouseMoved", x: 123, y: 456 },
    });
    assert.deepEqual(cdpInput(parseInput("9:5:1:2:123:456")!), {
        method: "Input.dispatchMouseEvent",
        params: {
            button: "left",
            clickCount: 2,
            modifiers: 0,
            type: "mousePressed",
            x: 123,
            y: 456,
        },
    });
    assert.deepEqual(cdpInput(parseInput("9:7:0:0:-120:240")!, { x: 321, y: 654 }), {
        method: "Input.dispatchMouseEvent",
        params: {
            deltaX: -120,
            deltaY: 240,
            modifiers: 0,
            type: "mouseWheel",
            x: 321,
            y: 654,
        },
    });
    assert.deepEqual(cdpInput(parseInput("9:3:0:0:955:0")!), {
        method: "Input.insertText",
        params: { text: "λ" },
    });
    assert.deepEqual(cdpInput(parseInput("9:1:4:2147483712:97:0")!), {
        method: "Input.dispatchKeyEvent",
        params: {
            autoRepeat: true,
            code: "a",
            key: "a",
            modifiers: 2,
            type: "keyDown",
        },
    });
});
