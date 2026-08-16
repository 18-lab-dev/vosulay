import type {
    BrowserWindowConstructorOptions,
    BrowserWindow as BrowserWindowType,
    Rectangle,
    WebContentsPaintEventParams,
} from "electron";
import {
    type NativeBitmapDescriptor,
    type NativeOverlayHandle,
    type NativeOverlaySurface,
    NativeSharedTexture,
} from "./linux-overlay-provider";
import type { ConfigStore } from "./config";
import type { EditingTarget } from "./types";
import EventEmitter from "node:events";
import { HostBridge } from "./host-bridge";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { ManagedOverlayProcess } from "./overlay-manager";
import type { RuntimeOptions } from "./options";
import { createRequire } from "node:module";
import path from "node:path";

const FALLBACK_WIDTH = 1280;
const FALLBACK_HEIGHT = 720;
const MANAGER_URL = "vosulay://manager/index.html";

const DEFAULT_POINTER_POSITION: PointerPosition = { x: 0, y: 0 };
const INPUT_FIELD_COUNT = 6;
const INPUT_FIELD_SEPARATOR = ":";
const INPUT_FLAG_REPEAT_BIT = 31;
const INPUT_FLAG_REPEAT = 1 << INPUT_FLAG_REPEAT_BIT;
const INPUT_KEY_DOWN = 1;
const INPUT_KEY_UP = 2;
const INPUT_MOUSE_MOVE = 4;
const INPUT_MOUSE_DOWN = 5;
const INPUT_MOUSE_UP = 6;
const INPUT_MOUSE_WHEEL = 7;
const INPUT_TEXT = 3;
const INPUT_CLICK_COUNT_MASK = 0xff;
const INPUT_FIELD_TARGET_ID_INDEX = 0;
const INPUT_FIELD_KIND_INDEX = 1;
const INPUT_FIELD_CODE_INDEX = 2;
const INPUT_FIELD_FLAGS_INDEX = 3;
const INPUT_FIELD_VALUE_INDEX = 4;
const INPUT_FIELD_VALUE2_INDEX = 5;
const MOUSE_BUTTON_LEFT_CODE = 1;
const MOUSE_BUTTON_MIDDLE_CODE = 2;
const MOUSE_BUTTON_RIGHT_CODE = 3;
const MODIFIER_SHIFT_LEFT = 0x00_01;
const MODIFIER_SHIFT_RIGHT = 0x00_02;
const MODIFIER_CONTROL_LEFT = 0x00_40;
const MODIFIER_CONTROL_RIGHT = 0x00_80;
const MODIFIER_ALT_LEFT = 0x01_00;
const MODIFIER_ALT_RIGHT = 0x02_00;
const MODIFIER_META_LEFT = 0x04_00;
const MODIFIER_META_RIGHT = 0x08_00;
const CDP_ALTERNATE_MODIFIERS_FLAG = 0x00_03;
const CDP_ALTERNATE_MODIFIERS_VALUE = 8;
const CDP_CONTROL_MODIFIERS_FLAG = 0x03_00;
const CDP_CONTROL_MODIFIERS_VALUE = 1;
const CDP_META_MODIFIERS_FLAG = 0x0c_00;
const CDP_META_MODIFIERS_VALUE = 4;
const CDP_SHIFT_MODIFIERS_FLAG = 0x00_c0;
const CDP_SHIFT_MODIFIERS_VALUE = 2;
const CDP_TEXT_MODIFIER_NONE = 0;
const SURROGATE_MIN = 55_296;
const SURROGATE_MAX = 57_343;
const UNICODE_MAX = 1_114_111;
const ASCII_SPACE_CODE_POINT = 0x20;
const ASCII_TILDE_CODE_POINT = 0x7e;
const TARGET_ACTIVE_ID_NONE = 0;
const TARGET_ID_MIN = 1;
const TARGET_CREATED_FIELDS = 4;
const TARGET_DESTROYED_FIELDS = 2;
const TARGET_SELECTED_FIELDS = 3;
const TARGET_KIND_INDEX = 0;
const TARGET_ID_INDEX = 1;
const TARGET_WIDTH_INDEX = 2;
const TARGET_HEIGHT_INDEX = 3;
const TARGET_GENERATION_INDEX = 2;
const TARGET_DESTROYED_KIND = "destroyed";
const TARGET_SELECTED_KIND = "selected";
const TARGET_CREATED_KIND = "created";
const TARGET_GENERATION_MIN = 1;
const MAX_SHARED_TEXTURE_INFLIGHT = 2;
const MAX_BITMAP_INFLIGHT = 2;
const SCANCODE_ALPHA_START = 4;
const SCANCODE_ALPHA_END = 29;
const SCANCODE_DIGIT_START = 30;
const SCANCODE_DIGIT_END = 38;
const SCANCODE_ZERO = 39;
const SCANCODE_ZERO_KEY_OFFSET = 29;
const SCANCODE_ENTER = 40;
const SCANCODE_ESCAPE = 41;
const SCANCODE_BACKSPACE = 42;
const SCANCODE_TAB = 43;
const SCANCODE_SPACE = 44;
const SCANCODE_F1 = 58;
const SCANCODE_INSERT = 73;
const SCANCODE_HOME = 74;
const SCANCODE_PAGE_UP = 75;
const SCANCODE_DELETE = 76;
const SCANCODE_END = 77;
const SCANCODE_PAGE_DOWN = 78;
const SCANCODE_RIGHT = 79;
const SCANCODE_LEFT = 80;
const SCANCODE_DOWN = 81;
const SCANCODE_UP = 82;
const KEY_A = 65;
const BYTE_COUNT_PER_PIXEL = 4;
const KEY_CODE_TEXT_KEY_MIN = 0;
const BOUNDS_LIMIT = 0;

const SPECIAL_SCANCODE_KEYS = new Map<number, string>([
    [SCANCODE_ENTER, "Enter"],
    [SCANCODE_ESCAPE, "Escape"],
    [SCANCODE_BACKSPACE, "Backspace"],
    [SCANCODE_TAB, "Tab"],
    [SCANCODE_SPACE, "Space"],
    [SCANCODE_INSERT, "Insert"],
    [SCANCODE_HOME, "Home"],
    [SCANCODE_PAGE_UP, "PageUp"],
    [SCANCODE_DELETE, "Delete"],
    [SCANCODE_END, "End"],
    [SCANCODE_PAGE_DOWN, "PageDown"],
    [SCANCODE_RIGHT, "Right"],
    [SCANCODE_LEFT, "Left"],
    [SCANCODE_DOWN, "Down"],
    [SCANCODE_UP, "Up"],
]);

const FUNCTION_KEY_LABELS = [
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
] as const;

const CdpKeyCodes = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"] as const;

export interface NativeTarget {
    id: number;
    width: number;
    height: number;
}

export type NativeTargetEvent =
    | { kind: "created"; target: NativeTarget }
    | { kind: "destroyed"; targetId: number }
    | { kind: "selected"; targetId: number; generation: number };

/// Which target overlay composites into is decided by the agent, which
/// knows the live drawables and announces through `selected`. Creation order
/// says nothing about it: a game can create a small offscreen drawable after
/// its real window.
export interface TargetState {
    readonly known: ReadonlyMap<number, NativeTarget>;
    readonly active: NativeTarget | undefined;
    readonly generation: number;
}

export const EMPTY_TARGET_STATE: TargetState = {
    active: undefined,
    generation: 0,
    known: new Map(),
};

interface NativeInputEvent {
    targetId: number;
    kind: number;
    code: number;
    flags: number;
    value: number;
    value2: number;
}

interface PointerPosition {
    x: number;
    y: number;
}

interface CdpMethodMouse {
    method: "Input.dispatchMouseEvent";
    params: CdpMouseMoveParams | CdpMouseButtonParams | CdpMouseWheelParams;
}

interface CdpMethodText {
    method: "Input.insertText";
    params: CdpTextParams;
}

interface CdpMethodKey {
    method: "Input.dispatchKeyEvent";
    params: CdpKeyParams;
}

type CdpEvent = CdpMethodMouse | CdpMethodText | CdpMethodKey;

interface CdpMouseCommon {
    modifiers: number;
    x: number;
    y: number;
}

interface CdpMouseWheelParams extends CdpMouseCommon {
    deltaX: number;
    deltaY: number;
    type: "mouseWheel";
}

interface CdpMouseMoveParams extends CdpMouseCommon {
    type: "mouseMoved";
}

interface CdpMouseButtonParams extends CdpMouseCommon {
    button: "left" | "middle" | "right";
    clickCount: number;
    type: "mousePressed" | "mouseReleased";
}

interface CdpTextParams {
    text: string;
}

interface CdpKeyParams {
    autoRepeat: boolean;
    code: string;
    key: string;
    modifiers: number;
    type: "keyDown" | "keyUp";
}

interface NativeBitmapLike {
    getSize: () => {
        height: number;
        width: number;
    };
    toBitmap: () => Buffer;
}

interface ElectronRuntime {
    app: { getAppPath: () => string };
    BrowserWindow: new (options?: BrowserWindowConstructorOptions) => BrowserWindowType;
}
function modifierNames(flags: number): Electron.InputEvent["modifiers"] {
    const modifiers: NonNullable<Electron.InputEvent["modifiers"]> = [];
    if (
        (flags & MODIFIER_SHIFT_LEFT) !== BOUNDS_LIMIT ||
        (flags & MODIFIER_SHIFT_RIGHT) !== BOUNDS_LIMIT
    ) {
        modifiers.push("shift");
    }
    if (
        (flags & MODIFIER_CONTROL_LEFT) !== BOUNDS_LIMIT ||
        (flags & MODIFIER_CONTROL_RIGHT) !== BOUNDS_LIMIT
    ) {
        modifiers.push("control");
    }
    if (
        (flags & MODIFIER_ALT_LEFT) !== BOUNDS_LIMIT ||
        (flags & MODIFIER_ALT_RIGHT) !== BOUNDS_LIMIT
    ) {
        modifiers.push("alt");
    }
    if (
        (flags & MODIFIER_META_LEFT) !== BOUNDS_LIMIT ||
        (flags & MODIFIER_META_RIGHT) !== BOUNDS_LIMIT
    ) {
        modifiers.push("meta");
    }
    return modifiers.length === 0 ? undefined : modifiers;
}

function cdpModifiers(flags: number): number {
    return (
        (flags & CDP_META_MODIFIERS_FLAG ? CDP_META_MODIFIERS_VALUE : BOUNDS_LIMIT) |
        (flags & CDP_CONTROL_MODIFIERS_FLAG ? CDP_CONTROL_MODIFIERS_VALUE : BOUNDS_LIMIT) |
        (flags & CDP_SHIFT_MODIFIERS_FLAG ? CDP_SHIFT_MODIFIERS_VALUE : BOUNDS_LIMIT) |
        (flags & CDP_ALTERNATE_MODIFIERS_FLAG ? CDP_ALTERNATE_MODIFIERS_VALUE : BOUNDS_LIMIT)
    );
}

export function parseInput(value: string): NativeInputEvent | undefined {
    const fields = value.split(INPUT_FIELD_SEPARATOR).map(Number);
    if (
        fields.length !== INPUT_FIELD_COUNT ||
        fields.some((field) => !Number.isSafeInteger(field))
    ) {
        return undefined;
    }
    const targetId = fields[INPUT_FIELD_TARGET_ID_INDEX];
    const kind = fields[INPUT_FIELD_KIND_INDEX];
    const code = fields[INPUT_FIELD_CODE_INDEX];
    const flags = fields[INPUT_FIELD_FLAGS_INDEX];
    const value0 = fields[INPUT_FIELD_VALUE_INDEX];
    const value1 = fields[INPUT_FIELD_VALUE2_INDEX];
    if (
        targetId === undefined ||
        kind === undefined ||
        code === undefined ||
        flags === undefined ||
        value0 === undefined ||
        value1 === undefined
    ) {
        return undefined;
    }
    return {
        code,
        flags,
        kind,
        targetId,
        value: value0,
        value2: value1,
    };
}

function mouseButton(code: number): "left" | "middle" | "right" | undefined {
    if (code === MOUSE_BUTTON_LEFT_CODE) {
        return "left";
    }
    if (code === MOUSE_BUTTON_MIDDLE_CODE) {
        return "middle";
    }
    if (code === MOUSE_BUTTON_RIGHT_CODE) {
        return "right";
    }
    return undefined;
}

function keyCode(scancode: number, logical: number): string | undefined {
    if (logical >= ASCII_SPACE_CODE_POINT && logical <= ASCII_TILDE_CODE_POINT) {
        return String.fromCodePoint(logical);
    }
    if (scancode >= SCANCODE_ALPHA_START && scancode <= SCANCODE_ALPHA_END) {
        return String.fromCodePoint(KEY_A + scancode - SCANCODE_ALPHA_START);
    }
    if (scancode >= SCANCODE_DIGIT_START && scancode <= SCANCODE_DIGIT_END) {
        return CdpKeyCodes[scancode - SCANCODE_ZERO_KEY_OFFSET];
    }
    if (scancode === SCANCODE_ZERO) {
        return "0";
    }
    if (scancode >= SCANCODE_F1 && scancode < SCANCODE_F1 + FUNCTION_KEY_LABELS.length) {
        return FUNCTION_KEY_LABELS[scancode - SCANCODE_F1];
    }
    return SPECIAL_SCANCODE_KEYS.get(scancode) ?? undefined;
}

export function electronInput(
    input: NativeInputEvent,
    pointer: PointerPosition = DEFAULT_POINTER_POSITION,
):
    | Electron.MouseInputEvent
    | Electron.MouseWheelInputEvent
    | Electron.KeyboardInputEvent
    | undefined {
    if (input.kind === INPUT_MOUSE_MOVE) {
        return { type: "mouseMove", x: input.value, y: input.value2 };
    }
    if (input.kind === INPUT_MOUSE_DOWN || input.kind === INPUT_MOUSE_UP) {
        const button = mouseButton(input.code);
        if (button === undefined) {
            return undefined;
        }
        return {
            button,
            clickCount: Math.max(1, input.flags & INPUT_CLICK_COUNT_MASK),
            type: input.kind === INPUT_MOUSE_DOWN ? "mouseDown" : "mouseUp",
            x: input.value,
            y: input.value2,
        };
    }
    if (input.kind === INPUT_MOUSE_WHEEL) {
        return {
            deltaX: input.value,
            deltaY: input.value2,
            type: "mouseWheel",
            x: pointer.x,
            y: pointer.y,
        };
    }
    if (input.kind === INPUT_TEXT) {
        if (
            input.value < KEY_CODE_TEXT_KEY_MIN ||
            input.value > UNICODE_MAX ||
            (input.value >= SURROGATE_MIN && input.value <= SURROGATE_MAX)
        ) {
            return undefined;
        }
        const text = String.fromCodePoint(input.value);
        return { keyCode: text, type: "char" };
    }
    if (input.kind === INPUT_KEY_DOWN || input.kind === INPUT_KEY_UP) {
        const key = keyCode(input.code, input.value);
        if (key === undefined) {
            return undefined;
        }
        const modifiers = modifierNames(input.flags);
        const event: Electron.KeyboardInputEvent = {
            keyCode: key,
            type: input.kind === INPUT_KEY_DOWN ? "keyDown" : "keyUp",
        };
        if (modifiers !== undefined) {
            event.modifiers = modifiers;
        }
        const repeat = (input.flags & INPUT_FLAG_REPEAT) !== BOUNDS_LIMIT;
        if (repeat) {
            event.modifiers = [...(modifiers ?? []), "isautorepeat"];
        }
        return event;
    }
    return undefined;
}
/// Retained as a pure translator for tests and diagnostics. Runtime delivery
/// uses `webContents.sendInputEvent`, avoiding the DevTools protocol attachment
/// and its per-event command allocation.
export function cdpInput(
    input: NativeInputEvent,
    pointer: PointerPosition = DEFAULT_POINTER_POSITION,
): CdpEvent | undefined {
    if (input.kind === INPUT_MOUSE_MOVE) {
        return {
            method: "Input.dispatchMouseEvent",
            params: {
                modifiers: CDP_TEXT_MODIFIER_NONE,
                type: "mouseMoved",
                x: input.value,
                y: input.value2,
            },
        };
    }
    if (input.kind === INPUT_MOUSE_DOWN || input.kind === INPUT_MOUSE_UP) {
        const button = mouseButton(input.code);
        if (button === undefined) {
            return undefined;
        }
        return {
            method: "Input.dispatchMouseEvent",
            params: {
                button,
                clickCount: Math.max(1, input.flags & INPUT_CLICK_COUNT_MASK),
                modifiers: CDP_TEXT_MODIFIER_NONE,
                type: input.kind === INPUT_MOUSE_DOWN ? "mousePressed" : "mouseReleased",
                x: input.value,
                y: input.value2,
            },
        };
    }
    if (input.kind === INPUT_MOUSE_WHEEL) {
        return {
            method: "Input.dispatchMouseEvent",
            params: {
                deltaX: input.value,
                deltaY: input.value2,
                modifiers: CDP_TEXT_MODIFIER_NONE,
                type: "mouseWheel",
                x: pointer.x,
                y: pointer.y,
            },
        };
    }
    if (input.kind === INPUT_TEXT) {
        if (
            input.value < KEY_CODE_TEXT_KEY_MIN ||
            input.value > UNICODE_MAX ||
            (input.value >= SURROGATE_MIN && input.value <= SURROGATE_MAX)
        ) {
            return undefined;
        }
        return { method: "Input.insertText", params: { text: String.fromCodePoint(input.value) } };
    }
    if (input.kind === INPUT_KEY_DOWN || input.kind === INPUT_KEY_UP) {
        const key = keyCode(input.code, input.value);
        if (key === undefined) {
            return undefined;
        }
        return {
            method: "Input.dispatchKeyEvent",
            params: {
                autoRepeat: (input.flags & INPUT_FLAG_REPEAT) !== BOUNDS_LIMIT,
                code: key,
                key,
                modifiers: cdpModifiers(input.flags),
                type: input.kind === INPUT_KEY_DOWN ? "keyDown" : "keyUp",
            },
        };
    }
    return undefined;
}

export function parseTargetEvent(value: string): NativeTargetEvent | undefined {
    const fields = value.split(INPUT_FIELD_SEPARATOR);
    const kind = fields[TARGET_KIND_INDEX];
    if (kind === TARGET_DESTROYED_KIND && fields.length === TARGET_DESTROYED_FIELDS) {
        const targetId = Number(fields[TARGET_ID_INDEX]);
        return Number.isSafeInteger(targetId) && targetId >= TARGET_ID_MIN
            ? { kind: TARGET_DESTROYED_KIND, targetId }
            : undefined;
    }
    if (kind === TARGET_SELECTED_KIND && fields.length === TARGET_SELECTED_FIELDS) {
        const generation = Number(fields[TARGET_GENERATION_INDEX]);
        const targetId = Number(fields[TARGET_ID_INDEX]);
        if (!Number.isSafeInteger(targetId) || targetId < TARGET_ACTIVE_ID_NONE) {
            return undefined;
        }
        if (!Number.isSafeInteger(generation) || generation < TARGET_GENERATION_MIN) {
            return undefined;
        }
        return { generation, kind: TARGET_SELECTED_KIND, targetId };
    }
    if (kind !== TARGET_CREATED_KIND || fields.length !== TARGET_CREATED_FIELDS) {
        return undefined;
    }
    const target = {
        height: Number(fields[TARGET_HEIGHT_INDEX]),
        id: Number(fields[TARGET_ID_INDEX]),
        width: Number(fields[TARGET_WIDTH_INDEX]),
    };
    if (
        !Number.isSafeInteger(target.id) ||
        target.id <= BOUNDS_LIMIT ||
        !Number.isSafeInteger(target.width) ||
        target.width <= BOUNDS_LIMIT ||
        !Number.isSafeInteger(target.height) ||
        target.height <= BOUNDS_LIMIT
    ) {
        return undefined;
    }
    return { kind: TARGET_CREATED_KIND, target };
}
/// Apply one target event. Registration and selection are deliberately
/// separate: creating a drawable only makes it known, and only the agent's
/// `selected` announcement changes which one is composited into. Treating
/// creation as selection made Electron and the agent disagree, so frames were
/// produced for one target while the agent composited another.
export function nextTargetState(state: TargetState, event: NativeTargetEvent): TargetState {
    if (event.kind === "created") {
        const known = new Map([...state.known, [event.target.id, event.target]]);
        const active = state.active?.id === event.target.id ? event.target : state.active;
        return { active, generation: state.generation, known };
    }
    if (event.kind === "destroyed") {
        if (!state.known.has(event.targetId) && state.active?.id !== event.targetId) {
            return state;
        }
        const known = new Map(state.known);
        known.delete(event.targetId);
        const active = state.active?.id === event.targetId ? undefined : state.active;
        return { active, generation: state.generation, known };
    }
    // Selections can be observed out of order; an older one must never undo a
    // Newer decision.
    if (event.generation <= state.generation) {
        return state;
    }
    const active =
        event.targetId === TARGET_ACTIVE_ID_NONE
            ? undefined
            : (state.known.get(event.targetId) ?? undefined);
    return { active, generation: event.generation, known: state.known };
}

function parseConsumed(value: string): number | undefined {
    const [, sequenceText] = value.split(INPUT_FIELD_SEPARATOR);
    const parsed = Number(sequenceText);
    return Number.isSafeInteger(parsed) && parsed > BOUNDS_LIMIT ? parsed : undefined;
}

type OffscreenPreference = true | { useSharedTexture: true; sharedTexturePixelFormat: "argb" };

/// A target whose graphics context cannot sample an imported DMA-BUF must be fed
/// CPU frames: requesting shared textures there yields a buffer the agent can
/// only render as opaque black. Addons without the query keep zero-copy.
export function offscreenPreference(
    native: Pick<NativeOverlayHandle, "supportsDmabuf">,
): OffscreenPreference {
    return (native.supportsDmabuf?.() ?? true)
        ? { sharedTexturePixelFormat: "argb", useSharedTexture: true }
        : true;
}

/// Chromium's CPU frame is premultiplied BGRA with tightly packed rows, which
/// the agent uploads directly, so no per-frame channel swizzle happens here.
export function bitmapFrame(
    targetId: number,
    image: NativeBitmapLike,
): { descriptor: NativeBitmapDescriptor; bitmap: Buffer } | undefined {
    const { height, width } = image.getSize();
    if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height)) {
        return undefined;
    }
    if (width <= BOUNDS_LIMIT || height <= BOUNDS_LIMIT) {
        return undefined;
    }
    const stride = width * BYTE_COUNT_PER_PIXEL;
    const bitmap = image.toBitmap();
    if (bitmap.length < stride * height) {
        return undefined;
    }
    return { bitmap, descriptor: { height, stride, targetId, width } };
}

/// Tracks resources acquired while building an overlay process so a failure
/// part-way through releases all of them.
///
/// Construction acquires a `BrowserWindow` before the native surface exists,
/// so until the `LinuxOverlayProcess` instance is built there is no `destroy()`
/// that could clean anything up. Each step registers its own rollback and a
/// failure unwinds them in reverse acquisition order.
export class StartupTransaction {
    private readonly rollbacks: (() => Promise<void> | void)[] = [];

    acquire(release: () => Promise<void> | void): void {
        this.rollbacks.push(release);
    }

    /// Ownership has transferred to a value that can release these itself.
    commit(): void {
        this.rollbacks.length = 0;
    }

    async unwind(): Promise<void> {
        for (let index = this.rollbacks.length - 1; index >= BOUNDS_LIMIT; index -= 1) {
            const rollback = this.rollbacks[index];
            if (rollback === undefined) {
                continue;
            }
            try {
                // oxlint-disable-next-line no-await-in-loop -- reverse-order teardown must finish in sequence
                await rollback();
            } catch {
                // Teardown failures must not mask the original error.
            }
        }
        this.rollbacks.length = BOUNDS_LIMIT;
    }
}

export class LinuxOverlayProcess implements ManagedOverlayProcess {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    readonly window: BrowserWindowType;
    readonly pid: number;

    private readonly bridge: HostBridge;
    private readonly surface: NativeOverlaySurface;
    private targets: TargetState = EMPTY_TARGET_STATE;
    private destroyed = false;
    private destroyedEmitted = false;
    private visible: boolean;
    private sequence = 0;
    private readonly releases = new Map<number, () => void>();
    /// Outstanding bitmap forwards. Same bound as DMA-BUF: Chromium paints
    /// faster than the agent can consume a full-frame SHM upload, and an
    /// unbounded queue copies ~14 MiB per paint into sealed memfds.
    private bitmapInflight = 0;
    private editing = false;
    private pointer: PointerPosition = { x: 0, y: 0 };
    private readonly diagnostics = process.env.VOSULAY_NATIVE_DIAGNOSTICS === "1";
    private firstFrameForwarded = false;
    private firstFrameConsumed = false;
    private firstPaintReported = false;

    private readonly native: NativeOverlayHandle;

    private constructor(
        pid: number,
        native: NativeOverlayHandle,
        window: BrowserWindowType,
        surface: NativeOverlaySurface,
        editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        visible: boolean,
    ) {
        this.pid = pid;
        this.native = native;
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
        /* SAFETY: The Electron module in this process must provide `app` and
         * `BrowserWindow`; native tests provide the same minimal shape. */
        const { app, BrowserWindow } = createRequire(import.meta.url)(
            "electron",
        ) as ElectronRuntime;
        const startup = new StartupTransaction();

        try {
            const { createSurface } = native;
            if (createSurface === undefined) {
                throw new Error("Native overlay does not expose createSurface()");
            }
            startup.acquire(() => native.destroy());

            const window = new BrowserWindow({
                backgroundColor: "#00000000",
                frame: false,
                show: false,
                transparent: true,
                webPreferences: {
                    additionalArguments: [`--vosulay-vosu-url=${options.vosuUrl.href}`],
                    backgroundThrottling: false,
                    contextIsolation: true,
                    nodeIntegration: false,
                    offscreen: offscreenPreference(native),
                    preload: path.join(app.getAppPath(), "out/preload/index.js"),
                    sandbox: true,
                    transparent: true,
                },
            });
            startup.acquire(() => {
                if (!window.isDestroyed()) {
                    window.destroy();
                }
            });
            window.setSize(FALLBACK_WIDTH, FALLBACK_HEIGHT, false);
            window.webContents.setFrameRate(options.fpsLimit);
            window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
            window.webContents.on("will-attach-webview", (event) => event.preventDefault());
            window.webContents.on("will-navigate", (event) => event.preventDefault());

            // The window already exists here, which is exactly why this step
            // Used to leak it when the native surface could not be created.
            const surface = await createSurface.call(native);
            const process = new LinuxOverlayProcess(
                pid,
                native,
                window,
                surface,
                editingTarget,
                options,
                configStore,
                overlayRegistry,
                visible,
            );
            // Ownership has transferred; `destroy()` now releases everything.
            startup.commit();
            try {
                await window.loadURL(MANAGER_URL);
                window.webContents.navigationHistory.clear();
                await native.setVisible?.(visible);
                process.updatePainting();
                return process;
            } catch (error) {
                await process.destroy();
                throw error;
            }
        } catch (error) {
            await startup.unwind();
            throw error;
        }
    }

    async destroy(): Promise<void> {
        if (this.destroyed) {
            return;
        }
        this.destroyed = true;
        let failure: unknown = undefined;
        this.window.webContents.stopPainting();
        this.window.webContents.off("paint", this.handlePaint);
        this.bridge.dispose();
        this.native.off?.("target", this.handleTarget);
        this.native.off?.("consumed", this.handleConsumed);
        this.native.off?.("disconnect", this.handleDisconnect);
        this.native.off?.("input", this.handleInput);
        this.native.off?.("end", this.handleInputEnd);
        try {
            await this.surface.clear?.(this.targets.active?.id ?? 0);
        } catch (error) {
            failure = error;
        }
        try {
            // The native side owns GPU retirement. Do not release Chromium's
            // Textures merely because JS is tearing down: a submitted GL/Vulkan
            // Draw may still be reading them. Once native teardown completes,
            // Anything still present here is either locally discarded or no
            // Longer reachable by the target process.
            await this.native.destroy();
        } catch (error) {
            failure ??= error;
        } finally {
            // A native session can end while a Vulkan swapchain image still
            // Retains the last submitted frame. Calling Chromium's release
            // Callback here would permit immediate producer reuse without a
            // GPU-completion proof. Drop our JS bookkeeping and let destroying
            // The offscreen window reclaim its renderer resources instead;
            // Locally discarded and completed frames were already released by
            // Their explicit `consumed` events.
            this.releases.clear();
            if (!this.window.isDestroyed()) {
                this.window.destroy();
            }
            this.emitDestroyed();
        }
        if (failure !== undefined) {
            throw failure;
        }
    }

    async setEditing(editing: boolean): Promise<void> {
        await (this.native.setEditing?.(editing) ?? this.native.captureInput?.(editing));
        this.editing = editing;
        await this.bridge.send({ editing, type: "editing" });
    }

    async setVisible(visible: boolean): Promise<void> {
        this.visible = visible;
        await this.native.setVisible?.(visible);
        if (!visible && this.targets.active !== undefined) {
            await this.surface.clear?.(this.targets.active.id);
        }
        this.updatePainting();
    }

    async flushPendingSave(): Promise<void> {
        if (!this.destroyed) {
            await this.bridge.requestSaveFlush();
        }
    }

    reload(): Promise<void> {
        this.window.webContents.reloadIgnoringCache();
        return Promise.resolve();
    }

    private updatePainting(): void {
        if (this.window.isDestroyed()) {
            return;
        }
        const shouldPaint = !this.destroyed && this.visible && this.targets.active !== undefined;
        if (shouldPaint && !this.window.webContents.isPainting()) {
            this.window.webContents.startPainting();
        } else if (!shouldPaint && this.window.webContents.isPainting()) {
            this.window.webContents.stopPainting();
        }
    }

    private readonly handleTarget = (value: string): void => {
        const event = parseTargetEvent(value);
        if (event === undefined || this.destroyed) {
            return;
        }
        const previous = this.targets.active;
        this.targets = nextTargetState(this.targets, event);
        const target = this.targets.active;
        const targetChanged = target?.id !== previous?.id;
        const geometryChanged =
            target !== undefined &&
            previous !== undefined &&
            target.id === previous.id &&
            (target.width !== previous.width || target.height !== previous.height);
        // Target selection/resize is not proof of GPU completion. Pending
        // Shared textures remain owned until the native agent explicitly emits
        // `consumed` (or reports a local discard before the frame was sent).
        if (
            target !== undefined &&
            (targetChanged || geometryChanged) &&
            !this.window.isDestroyed()
        ) {
            this.window.setSize(target.width, target.height, false);
        }
        this.updatePainting();
        if (targetChanged && target !== undefined && this.diagnostics) {
            console.info(
                `Native overlay target for PID ${this.pid}: ${target.id} ${target.width}x${target.height}`,
            );
        }
    };

    private readonly handleConsumed = (value: string): void => {
        const sequence = parseConsumed(value);
        if (sequence === undefined) {
            return;
        }
        this.releases.get(sequence)?.();
        this.releases.delete(sequence);
        if (this.diagnostics && !this.firstFrameConsumed) {
            this.firstFrameConsumed = true;
            console.info(`Native overlay consumed frame ${sequence} for PID ${this.pid}`);
        }
    };
    private readonly handleInput = (value: string): void => {
        const input = parseInput(value);
        if (
            input === undefined ||
            input.targetId !== this.targets.active?.id ||
            this.destroyed ||
            !this.editing
        ) {
            return;
        }
        if (
            input.kind === INPUT_MOUSE_MOVE ||
            input.kind === INPUT_MOUSE_DOWN ||
            input.kind === INPUT_MOUSE_UP
        ) {
            this.pointer = { x: input.value, y: input.value2 };
        }
        const event = electronInput(input, this.pointer);
        if (event === undefined) {
            return;
        }
        try {
            this.window.webContents.sendInputEvent(event);
        } catch (error) {
            console.error(`Failed to deliver captured input for PID ${this.pid}`, error);
        }
    };

    private readonly handleInputEnd = (): void => {
        if (!this.destroyed && this.editing) {
            this.editing = false;
            void this.bridge.send({ editing: false, type: "editing" });
        }
    };

    private readonly handleDisconnect = (): void => {
        void this.destroy().catch((cause: unknown) => {
            console.error(`Failed to tear down disconnected overlay for PID ${this.pid}`, cause);
        });
    };
    private readonly handlePaint = (
        event: Electron.Event<WebContentsPaintEventParams>,
        _dirtyRect: Rectangle,
        image: Electron.NativeImage,
    ): void => {
        const { texture } = event;
        if (this.diagnostics && !this.firstPaintReported) {
            this.firstPaintReported = true;
            const size = image.getSize();
            console.info(
                `Native overlay paint: transport=${texture === undefined ? "bitmap" : "shared-texture"}` +
                    ` size=${size.width}x${size.height} target=${this.targets.active !== undefined}` +
                    ` visible=${this.visible}`,
            );
        }
        const target = this.targets.active;
        if (this.destroyed || !this.visible || target === undefined) {
            texture?.release();
            return;
        }
        if (texture === undefined) {
            this.forwardBitmap(target, image);
            return;
        }
        if (this.releases.size >= MAX_SHARED_TEXTURE_INFLIGHT) {
            texture.release();
            return;
        }
        const { release } = texture;
        const { textureInfo } = texture;
        const descriptor = NativeSharedTexture.fromElectron(
            target.id,
            ++this.sequence,
            textureInfo,
            _dirtyRect,
        );
        if (descriptor === undefined) {
            release();
            return;
        }
        const { updateDmabuf } = this.surface;
        if (updateDmabuf === undefined) {
            release();
            return;
        }
        this.releases.set(descriptor.sequence, release);
        if (this.diagnostics && !this.firstFrameForwarded) {
            const { pixelFormat, widgetType, codedSize } = textureInfo;
            const { modifier, fourcc, planes } = descriptor;
            const { width, height } = codedSize;
            console.info(
                `Native overlay surface: format=${pixelFormat} widget=${widgetType}` +
                    ` coded=${width}x${height}` +
                    ` modifier=${modifier} fourcc=${fourcc}` +
                    ` planes=${planes.length}`,
            );
        }
        // Start from an already-resolved promise so a synchronous N-API throw
        // Takes the same cleanup path as an asynchronous rejection.
        void Promise.resolve()
            .then(() => updateDmabuf.call(this.surface, descriptor))
            .then(() => {
                if (this.diagnostics && !this.firstFrameForwarded) {
                    this.firstFrameForwarded = true;
                    console.info(
                        `Forwarded native overlay DMA-BUF frame ${descriptor.sequence} for PID ${this.pid}`,
                    );
                }
            })
            .catch((cause: unknown) => {
                this.releases.get(descriptor.sequence)?.();
                this.releases.delete(descriptor.sequence);
                console.error(`Failed to forward overlay frame for PID ${this.pid}`, cause);
            });
    };

    /// Compatibility path: Chromium's CPU frame is premultiplied BGRA, which the
    /// agent uploads directly, so no channel swizzle happens per frame here.
    private forwardBitmap(target: NativeTarget, image: Electron.NativeImage): void {
        const { updateBitmap } = this.surface;
        if (updateBitmap === undefined) {
            return;
        }
        if (this.bitmapInflight >= MAX_BITMAP_INFLIGHT) {
            return;
        }
        const frame = bitmapFrame(target.id, image);
        if (frame === undefined) {
            return;
        }
        const { descriptor, bitmap } = frame;
        const { height, width } = descriptor;
        const sequence = ++this.sequence;
        this.bitmapInflight += 1;
        void Promise.resolve()
            .then(() => updateBitmap.call(this.surface, descriptor, bitmap))
            .then(() => {
                if (this.diagnostics && !this.firstFrameForwarded) {
                    this.firstFrameForwarded = true;
                    console.info(
                        `Forwarded native overlay bitmap frame ${sequence} (${width}x${height})` +
                            ` for PID ${this.pid}`,
                    );
                }
            })
            .catch((cause: unknown) => {
                console.error(`Failed to forward overlay bitmap for PID ${this.pid}`, cause);
            })
            .finally(() => {
                this.bitmapInflight = Math.max(0, this.bitmapInflight - 1);
            });
    }
    private emitDestroyed(): void {
        if (this.destroyedEmitted) {
            return;
        }
        this.destroyedEmitted = true;
        this.events.emit("destroyed");
    }
}
