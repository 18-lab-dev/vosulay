import EventEmitter from "node:events";
import path from "node:path";

import { createRequire } from "node:module";
import type {
    BrowserWindow as BrowserWindowType,
    BrowserWindowConstructorOptions,
    Rectangle,
    WebContentsPaintEventParams,
} from "electron";

import type { ConfigStore } from "./config";
import { HostBridge } from "./host-bridge";
import type { LocalOverlayRegistry } from "./local-overlays";
import {
    NativeSharedTexture,
    type NativeBitmapDescriptor,
    type NativeOverlayHandle,
    type NativeOverlaySurface,
} from "./linux-overlay-provider";
import type { ManagedOverlayProcess } from "./overlay-manager";
import type { RuntimeOptions } from "./options";
import type { EditingTarget } from "./types";

const MANAGER_URL = "vosulay://manager/index.html";
const FALLBACK_WIDTH = 1280;
const FALLBACK_HEIGHT = 720;

export interface NativeTarget {
    id: number;
    width: number;
    height: number;
}
export type NativeTargetEvent =
    | { kind: "created"; target: NativeTarget }
    | { kind: "destroyed"; targetId: number };
interface NativeInputEvent {
    targetId: number;
    kind: number;
    code: number;
    flags: number;
    value: number;
    value2: number;
}

const INPUT_KEY_DOWN = 1;
const INPUT_KEY_UP = 2;
const INPUT_TEXT = 3;
const INPUT_MOUSE_MOVE = 4;
const INPUT_MOUSE_DOWN = 5;
const INPUT_MOUSE_UP = 6;
const INPUT_MOUSE_WHEEL = 7;
const INPUT_FLAG_REPEAT = 1 << 31;

function modifierNames(flags: number): Electron.InputEvent["modifiers"] {
    const modifiers: NonNullable<Electron.InputEvent["modifiers"]> = [];
    if ((flags & 0x0001) !== 0 || (flags & 0x0002) !== 0) modifiers.push("shift");
    if ((flags & 0x0040) !== 0 || (flags & 0x0080) !== 0) modifiers.push("control");
    if ((flags & 0x0100) !== 0 || (flags & 0x0200) !== 0) modifiers.push("alt");
    if ((flags & 0x0400) !== 0 || (flags & 0x0800) !== 0) modifiers.push("meta");
    return modifiers.length === 0 ? undefined : modifiers;
}

export function parseInput(value: unknown): NativeInputEvent | null {
    if (typeof value !== "string") return null;
    const fields = value.split(":").map(Number);
    if (fields.length !== 6 || fields.some((field) => !Number.isSafeInteger(field))) return null;
    const [targetId, kind, code, flags, first, second] = fields;
    if (targetId === undefined || kind === undefined || code === undefined
        || flags === undefined || first === undefined || second === undefined) return null;
    return { targetId, kind, code, flags, value: first, value2: second };
}

function mouseButton(code: number): "left" | "middle" | "right" | undefined {
    if (code === 1) return "left";
    if (code === 2) return "middle";
    if (code === 3) return "right";
    return undefined;
}

function keyCode(scancode: number, logical: number): string | null {
    if (logical >= 32 && logical <= 126) return String.fromCodePoint(logical);
    if (scancode >= 4 && scancode <= 29) return String.fromCharCode(65 + scancode - 4);
    if (scancode >= 30 && scancode <= 38) return String(scancode - 29);
    if (scancode === 39) return "0";
    return new Map<number, string>([
        [40, "Enter"], [41, "Escape"], [42, "Backspace"], [43, "Tab"], [44, "Space"],
        [58, "F1"], [59, "F2"], [60, "F3"], [61, "F4"], [62, "F5"], [63, "F6"],
        [64, "F7"], [65, "F8"], [66, "F9"], [67, "F10"], [68, "F11"], [69, "F12"],
        [73, "Insert"], [74, "Home"], [75, "PageUp"], [76, "Delete"], [77, "End"],
        [78, "PageDown"], [79, "Right"], [80, "Left"], [81, "Down"], [82, "Up"],
    ]).get(scancode) ?? null;
}

export function electronInput(
    input: NativeInputEvent,
): Electron.MouseInputEvent | Electron.MouseWheelInputEvent | Electron.KeyboardInputEvent | null {
    if (input.kind === INPUT_MOUSE_MOVE) {
        return { type: "mouseMove", x: input.value, y: input.value2 };
    }
    if (input.kind === INPUT_MOUSE_DOWN || input.kind === INPUT_MOUSE_UP) {
        const button = mouseButton(input.code);
        if (button === undefined) return null;
        return {
            type: input.kind === INPUT_MOUSE_DOWN ? "mouseDown" : "mouseUp",
            x: input.value,
            y: input.value2,
            button,
            clickCount: Math.max(1, input.flags),
        };
    }
    if (input.kind === INPUT_MOUSE_WHEEL) {
        return {
            type: "mouseWheel",
            x: 0,
            y: 0,
            deltaX: input.value,
            deltaY: input.value2,
        };
    }
    if (input.kind === INPUT_TEXT) {
        if (input.value < 0 || input.value > 0x10ffff
            || (input.value >= 0xd800 && input.value <= 0xdfff)) return null;
        const text = String.fromCodePoint(input.value);
        return { type: "char", keyCode: text };
    }
    if (input.kind === INPUT_KEY_DOWN || input.kind === INPUT_KEY_UP) {
        const key = keyCode(input.code, input.value);
        if (key === null) return null;
        const event: Electron.KeyboardInputEvent = {
            type: input.kind === INPUT_KEY_DOWN ? "keyDown" : "keyUp",
            keyCode: key,
        };
        let modifiers = modifierNames(input.flags);
        if ((input.flags & INPUT_FLAG_REPEAT) !== 0) {
            (modifiers ??= []).push("isautorepeat");
        }
        if (modifiers !== undefined) event.modifiers = modifiers;
        return event;
    }
    return null;
}
export function cdpInput(input: NativeInputEvent): {
    method: string;
    params: Record<string, unknown>;
} | null {
    const modifiers = (input.flags & 0x0c00 ? 4 : 0)
        | (input.flags & 0x0003 ? 8 : 0)
        | (input.flags & 0x00c0 ? 2 : 0)
        | (input.flags & 0x0300 ? 1 : 0);
    if (input.kind === INPUT_MOUSE_MOVE) {
        return { method: "Input.dispatchMouseEvent", params: {
            type: "mouseMoved", x: input.value, y: input.value2, modifiers,
        } };
    }
    if (input.kind === INPUT_MOUSE_DOWN || input.kind === INPUT_MOUSE_UP) {
        const button = mouseButton(input.code);
        if (button === undefined) return null;
        return { method: "Input.dispatchMouseEvent", params: {
            type: input.kind === INPUT_MOUSE_DOWN ? "mousePressed" : "mouseReleased",
            x: input.value, y: input.value2, button,
            clickCount: Math.max(1, input.flags & 0xff), modifiers,
        } };
    }
    if (input.kind === INPUT_MOUSE_WHEEL) {
        return { method: "Input.dispatchMouseEvent", params: {
            type: "mouseWheel", x: 0, y: 0,
            deltaX: input.value, deltaY: input.value2, modifiers,
        } };
    }
    if (input.kind === INPUT_TEXT) {
        if (input.value < 0 || input.value > 0x10ffff
            || (input.value >= 0xd800 && input.value <= 0xdfff)) return null;
        return { method: "Input.insertText", params: { text: String.fromCodePoint(input.value) } };
    }
    if (input.kind === INPUT_KEY_DOWN || input.kind === INPUT_KEY_UP) {
        const key = keyCode(input.code, input.value);
        if (key === null) return null;
        return { method: "Input.dispatchKeyEvent", params: {
            type: input.kind === INPUT_KEY_DOWN ? "keyDown" : "keyUp",
            key, code: key, modifiers,
            autoRepeat: (input.flags & INPUT_FLAG_REPEAT) !== 0,
        } };
    }
    return null;
}


interface ElectronRuntime {
    app: { getAppPath(): string };
    BrowserWindow: new (options?: BrowserWindowConstructorOptions) => BrowserWindowType;
}

export function parseTargetEvent(value: unknown): NativeTargetEvent | null {
    if (typeof value !== "string") return null;
    const fields = value.split(":");
    if (fields[0] === "destroyed" && fields.length === 2) {
        const targetId = Number(fields[1]);
        return Number.isSafeInteger(targetId) && targetId > 0
            ? { kind: "destroyed", targetId }
            : null;
    }
    if (fields[0] !== "created" || fields.length !== 4) return null;
    const target = { id: Number(fields[1]), width: Number(fields[2]), height: Number(fields[3]) };
    if (!Number.isSafeInteger(target.id) || target.id <= 0
        || !Number.isSafeInteger(target.width) || target.width <= 0
        || !Number.isSafeInteger(target.height) || target.height <= 0) return null;
    return { kind: "created", target };
}

export function nextTarget(
    current: NativeTarget | null,
    event: NativeTargetEvent,
): NativeTarget | null {
    if (event.kind === "created") return event.target;
    return current?.id === event.targetId ? null : current;
}

function parseConsumed(value: unknown): number | null {
    if (typeof value !== "string") return null;
    const [, sequence] = value.split(":");
    const parsed = Number(sequence);
    return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : null;
}

type OffscreenPreference = true | { useSharedTexture: true; sharedTexturePixelFormat: "argb" };

/// A target whose graphics context cannot sample an imported DMA-BUF must be fed
/// CPU frames: requesting shared textures there yields a buffer the agent can
/// only render as opaque black. Addons without the query keep zero-copy.
export function offscreenPreference(
    native: Pick<NativeOverlayHandle, "supportsDmabuf">,
): OffscreenPreference {
    return (native.supportsDmabuf?.() ?? true)
        ? { useSharedTexture: true, sharedTexturePixelFormat: "argb" }
        : true;
}

/// Chromium's CPU frame is premultiplied BGRA with tightly packed rows, which
/// the agent uploads directly, so no per-frame channel swizzle happens here.
export function bitmapFrame(
    targetId: number,
    image: { getSize(): { width: number; height: number }; toBitmap(): Buffer },
): { descriptor: NativeBitmapDescriptor; bitmap: Buffer } | null {
    const { width, height } = image.getSize();
    if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height)) return null;
    if (width <= 0 || height <= 0) return null;
    const stride = width * 4;
    const bitmap = image.toBitmap();
    if (bitmap.length < stride * height) return null;
    return { descriptor: { targetId, width, height, stride }, bitmap };
}

export class LinuxOverlayProcess implements ManagedOverlayProcess {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    readonly window: BrowserWindowType;
    readonly pid: number;

    private readonly bridge: HostBridge;
    private readonly surface: NativeOverlaySurface;
    private target: NativeTarget | null = null;
    private destroyed = false;
    private destroyedEmitted = false;
    private visible: boolean;
    private sequence = 0;
    private readonly releases = new Map<number, () => void>();
    /// Outstanding bitmap forwards. Same bound as DMA-BUF: Chromium paints
    /// faster than the agent can consume a full-frame SHM upload, and an
    /// unbounded queue copies ~14 MiB per paint into sealed memfds.
    private bitmapInflight = 0;
    private debuggerAttached = false;
    private editing = false;
    private readonly diagnostics = process.env.VOSULAY_NATIVE_DIAGNOSTICS === "1";
    private firstFrameForwarded = false;
    private firstFrameConsumed = false;
    private firstPaintReported = false;

    private constructor(
        pid: number,
        private readonly native: NativeOverlayHandle,
        window: BrowserWindowType,
        surface: NativeOverlaySurface,
        editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        visible: boolean,
    ) {
        this.pid = pid;
        this.window = window;
        this.surface = surface;
        this.visible = visible;
        this.bridge = new HostBridge(
            window.webContents,
            editingTarget,
            options,
            configStore,
            overlayRegistry,
        );
        native.on?.("target", this.handleTarget);
        native.on?.("consumed", this.handleConsumed);
        native.on?.("disconnect", this.handleDisconnect);
        native.on?.("input", this.handleInput);
        native.on?.("end", this.handleInputEnd);
        window.webContents.on("paint", this.handlePaint);
        window.on("closed", () => void this.destroy());
        this.bridge.start();
        window.webContents.debugger.on("detach", () => {
            this.debuggerAttached = false;
        });
    }

    static async create(
        pid: number,
        native: NativeOverlayHandle,
        editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        visible: boolean,
    ): Promise<LinuxOverlayProcess> {
        // Electron is a runtime facade in Node tests and the real module in the packaged main process.
        const { app, BrowserWindow } = createRequire(import.meta.url)("electron") as ElectronRuntime;
        const window = new BrowserWindow({
            show: false,
            frame: false,
            transparent: true,
            backgroundColor: "#00000000",
            webPreferences: {
                preload: path.join(app.getAppPath(), "out/preload/index.js"),
                contextIsolation: true,
                sandbox: true,
                nodeIntegration: false,
                offscreen: offscreenPreference(native),
                transparent: true,
                backgroundThrottling: false,
                additionalArguments: [`--vosulay-vosu-url=${options.vosuUrl.href}`],
            },
        });
        window.setSize(FALLBACK_WIDTH, FALLBACK_HEIGHT, false);
        window.webContents.setFrameRate(options.fpsLimit);
        window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
        window.webContents.on("will-attach-webview", (event) => event.preventDefault());
        window.webContents.on("will-navigate", (event) => event.preventDefault());

        const createSurface = native.createSurface;
        if (createSurface === undefined) throw new Error("Native overlay does not expose createSurface()");
        const process = new LinuxOverlayProcess(
            pid,
            native,
            window,
            await createSurface.call(native),
            editingTarget,
            options,
            configStore,
            overlayRegistry,
            visible,
        );
        try {
            await window.loadURL(MANAGER_URL);
            window.webContents.navigationHistory.clear();
            window.webContents.debugger.attach("1.3");
            process.debuggerAttached = true;
            await native.setVisible?.(visible);
            return process;
        } catch (error) {
            await process.destroy();
            throw error;
        }
    }

    async destroy(): Promise<void> {
        if (this.destroyed) return;
        this.destroyed = true;
        let failure: unknown;
        this.window.webContents.off("paint", this.handlePaint);
        this.bridge.dispose();
        this.native.off?.("target", this.handleTarget);
        this.native.off?.("consumed", this.handleConsumed);
        this.native.off?.("disconnect", this.handleDisconnect);
        this.native.off?.("input", this.handleInput);
        this.native.off?.("end", this.handleInputEnd);
        if (this.debuggerAttached && this.window.webContents.debugger.isAttached()) {
            this.window.webContents.debugger.detach();
        }
        try {
            await this.surface.clear?.(this.target?.id ?? 0);
        } catch (error) {
            failure = error;
        }
        for (const release of this.releases.values()) release();
        this.releases.clear();
        try {
            await this.native.destroy();
        } catch (error) {
            failure ??= error;
        } finally {
            if (!this.window.isDestroyed()) this.window.destroy();
            this.emitDestroyed();
        }
        if (failure !== undefined) throw failure;
    }

    async setEditing(editing: boolean): Promise<void> {
        await (this.native.setEditing?.(editing) ?? this.native.captureInput?.(editing));
        this.editing = editing;
        await this.bridge.send({ type: "editing", editing });
    }

    async setVisible(visible: boolean): Promise<void> {
        this.visible = visible;
        await this.native.setVisible?.(visible);
        if (!visible && this.target !== null) await this.surface.clear?.(this.target.id);
    }

    async flushPendingSave(): Promise<void> {
        if (!this.destroyed) await this.bridge.requestSaveFlush();
    }

    async reload(): Promise<void> {
        this.window.webContents.reloadIgnoringCache();
    }

    private readonly handleTarget = (value: unknown): void => {
        const event = parseTargetEvent(value);
        if (event === null || this.destroyed) return;
        const target = nextTarget(this.target, event);
        if (target === this.target) return;
        this.releasePendingTextures();
        this.target = target;
        if (target !== null && !this.window.isDestroyed()) {
            this.window.setSize(target.width, target.height, false);
        }
        if (target !== null && this.diagnostics) {
            console.info(
                `Native overlay target for PID ${this.pid}: ${target.id} ${target.width}x${target.height}`,
            );
        }
    };

    private readonly handleConsumed = (value: unknown): void => {
        const sequence = parseConsumed(value);
        if (sequence === null) return;
        this.releases.get(sequence)?.();
        this.releases.delete(sequence);
        if (this.diagnostics && !this.firstFrameConsumed) {
            this.firstFrameConsumed = true;
            console.info(`Native overlay consumed frame ${sequence} for PID ${this.pid}`);
        }
    };

    private releasePendingTextures(): void {
        for (const release of this.releases.values()) release();
        this.releases.clear();
    }

    private readonly handleInput = (value: unknown): void => {
        const input = parseInput(value);
        if (input === null || input.targetId !== this.target?.id || this.destroyed || !this.editing) {
            return;
        }
        const command = cdpInput(input);
        if (command === null || !this.debuggerAttached) return;
        void this.window.webContents.debugger
            .sendCommand(command.method, command.params)
            .catch((error: unknown) => {
                console.error(`Failed to deliver captured input for PID ${this.pid}`, error);
            });
    };

    private readonly handleInputEnd = (): void => {
        if (!this.destroyed && this.editing) {
            this.editing = false;
            void this.bridge.send({ type: "editing", editing: false });
        }
    };

    private readonly handleDisconnect = (): void => {
        void this.destroy().catch((error: unknown) => {
            console.error(`Failed to tear down disconnected overlay for PID ${this.pid}`, error);
        });
    };

    private readonly handlePaint = (
        event: Electron.Event<WebContentsPaintEventParams>,
        _dirtyRect: Rectangle,
        image: Electron.NativeImage,
    ): void => {
        const texture = event.texture;
        if (this.diagnostics && !this.firstPaintReported) {
            this.firstPaintReported = true;
            const size = image.getSize();
            console.info(
                `Native overlay paint: transport=${texture === undefined ? "bitmap" : "shared-texture"}`
                + ` size=${size.width}x${size.height} target=${this.target !== null}`
                + ` visible=${this.visible}`,
            );
        }
        const target = this.target;
        if (this.destroyed || !this.visible || target === null) {
            texture?.release();
            return;
        }
        if (texture === undefined) {
            this.forwardBitmap(target, image);
            return;
        }
        if (this.releases.size >= 2) {
            texture.release();
            return;
        }
        const descriptor = NativeSharedTexture.fromElectron(
            target.id,
            ++this.sequence,
            texture.textureInfo,
            _dirtyRect,
        );
        if (descriptor === null) {
            texture.release();
            return;
        }
        this.releases.set(descriptor.sequence, texture.release);
        if (this.diagnostics && !this.firstFrameForwarded) {
            const info = texture.textureInfo;
            console.info(
                `Native overlay surface: format=${info.pixelFormat} widget=${info.widgetType}`
                + ` coded=${info.codedSize.width}x${info.codedSize.height}`
                + ` modifier=${descriptor.modifier} fourcc=${descriptor.fourcc}`
                + ` planes=${descriptor.planes.length}`,
            );
        }
        void Promise.resolve(this.surface.updateDmabuf(descriptor))
            .then(() => {
                if (this.diagnostics && !this.firstFrameForwarded) {
                    this.firstFrameForwarded = true;
                    console.info(
                        `Forwarded native overlay DMA-BUF frame ${descriptor.sequence} for PID ${this.pid}`,
                    );
                }
            })
            .catch((error: unknown) => {
                this.releases.get(descriptor.sequence)?.();
                this.releases.delete(descriptor.sequence);
                console.error(`Failed to forward overlay frame for PID ${this.pid}`, error);
            });
    };

    /// Compatibility path: Chromium's CPU frame is premultiplied BGRA, which the
    /// agent uploads directly, so no channel swizzle happens per frame here.
    private forwardBitmap(target: NativeTarget, image: Electron.NativeImage): void {
        const update = this.surface.updateBitmap;
        if (update === undefined) return;
        if (this.bitmapInflight >= 2) return;
        const frame = bitmapFrame(target.id, image);
        if (frame === null) return;
        const { descriptor, bitmap } = frame;
        const { width, height } = descriptor;
        const sequence = ++this.sequence;
        this.bitmapInflight += 1;
        void Promise.resolve(
            update.call(this.surface, descriptor, bitmap),
        )
            .then(() => {
                if (this.diagnostics && !this.firstFrameForwarded) {
                    this.firstFrameForwarded = true;
                    console.info(
                        `Forwarded native overlay bitmap frame ${sequence} (${width}x${height})`
                        + ` for PID ${this.pid}`,
                    );
                }
            })
            .catch((error: unknown) => {
                console.error(`Failed to forward overlay bitmap for PID ${this.pid}`, error);
            })
            .finally(() => {
                this.bitmapInflight = Math.max(0, this.bitmapInflight - 1);
            });
    }

    private emitDestroyed(): void {
        if (this.destroyedEmitted) return;
        this.destroyedEmitted = true;
        this.events.emit("destroyed");
    }
}
