import {
    BrowserWindow,
    type Event as ElectronEvent,
    type Input,
    type WebContents,
    app,
} from "electron";
import {
    type EditShortcutCommand,
    type NativeEditShortcutInput,
    NativeEditShortcutState,
    editShortcutCommand,
} from "./edit-shortcut";
import { type GpuLuid, type OverlayTarget, chooseOverlayTarget } from "./overlay-target";
import { fatalProcessExit, withFatalDeadline } from "./fatal-deadline";
import type { ConfigStore } from "./config";
import type { EditingTarget } from "./types";
import EventEmitter from "node:events";
import { HostBridge } from "./host-bridge";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import { createRequire } from "node:module";
import path from "node:path";

type OverlayEventListener<Args extends unknown[]> = (...args: Args) => void;

interface OverlayEventPayloads {
    added: [number, number, number, GpuLuid];
    resized: [number, number, number];
    destroyed: [number];
    error: [unknown];
    disconnected: [];
    keyboard_input: [number, KeyboardInput];
    input_blocking_ended: [number];
}

type OverlayEventName = keyof OverlayEventPayloads;

interface OverlayEvent {
    on: <EventName extends OverlayEventName>(
        event: EventName,
        listener: OverlayEventListener<OverlayEventPayloads[EventName]>,
    ) => void;
    off: <EventName extends OverlayEventName>(
        event: EventName,
        listener: OverlayEventListener<OverlayEventPayloads[EventName]>,
    ) => void;
}

interface OverlayInstance {
    readonly event: OverlayEvent;
    blockInput: (hwnd: number, blocked: boolean) => Promise<void> | void;
    destroy: () => void;
    setPosition: (hwnd: number, x: number, y: number) => Promise<void> | void;
    setAnchor: (hwnd: number, x: number, y: number) => Promise<void> | void;
    setMargin: (
        hwnd: number,
        top: number,
        right: number,
        bottom: number,
        left: number,
    ) => Promise<void> | void;
}

interface OverlayModule {
    attach: (dllDirectory: string, pid: number, timeoutMs: number) => Promise<OverlayInstance>;
}

interface OverlayInput {
    disconnect: () => void;
}

interface OverlayInputModule {
    connect: (
        target: { readonly id: number; readonly overlay: OverlayInstance },
        webContents: WebContents,
    ) => OverlayInput;
}

interface SurfaceEventPayloads {
    error: [unknown];
}

type SurfaceEventName = keyof SurfaceEventPayloads;

interface SurfaceEvents {
    on: <EventName extends SurfaceEventName>(
        event: EventName,
        listener: OverlayEventListener<SurfaceEventPayloads[EventName]>,
    ) => void;
    off: <EventName extends SurfaceEventName>(
        event: EventName,
        listener: OverlayEventListener<SurfaceEventPayloads[EventName]>,
    ) => void;
}

interface OverlaySurface {
    readonly events: SurfaceEvents;
    disconnect: () => Promise<void> | void;
}

interface OverlaySurfaceModule {
    connect: (
        target: { readonly id: number; readonly overlay: OverlayInstance },
        luid: GpuLuid,
        webContents: WebContents,
    ) => OverlaySurface;
}

interface WindowsOverlayRuntime {
    readonly overlay: OverlayModule;
    readonly defaultDllDir: () => string;
    readonly length: (value: number) => number;
    readonly input: OverlayInputModule;
    readonly surface: OverlaySurfaceModule;
}

interface KeyboardInput extends NativeEditShortcutInput {
    readonly key: { readonly code: number };
    readonly state: string;
}
interface TimeoutState {
    settleTimeout?: ReturnType<typeof setTimeout>;
    timeout?: ReturnType<typeof setTimeout>;
}

const loadWindowsOverlayRuntime = (): WindowsOverlayRuntime => {
    const require = createRequire(import.meta.url);
    /* SAFETY: The add-on's shape is stable for the runtime versions this process ships with. */
    const core = require("@asdf-overlay/core") as {
        readonly Overlay: OverlayModule;
        readonly defaultDllDir: () => string;
        readonly length: (value: number) => number;
    };
    /* SAFETY: The add-on exposes the documented Electron overlay input binding under this key. */
    const input = require("@asdf-overlay/electron/input") as {
        readonly ElectronOverlayInput: OverlayInputModule;
    };
    /* SAFETY: The add-on exposes the documented Electron overlay surface binding under this key. */
    const surface = require("@asdf-overlay/electron/surface") as {
        readonly ElectronOverlaySurface: OverlaySurfaceModule;
    };
    return {
        defaultDllDir: core.defaultDllDir,
        input: input.ElectronOverlayInput,
        length: core.length,
        overlay: core.Overlay,
        surface: surface.ElectronOverlaySurface,
    };
};
const MANAGER_URL = "vosulay://manager/index.html";
const PRELOAD_PATH = path.join(app.getAppPath(), "out/preload/index.js");
const INPUT_REQUEST_TIMEOUT_MS = 5000;
const OVERLAY_ATTACH_TIMEOUT_MS = 5000;
const OVERLAY_WINDOW_SETTLE_MS = 250;
const OVERLAY_WINDOW_TIMEOUT_MS = 30_000;
const INPUT_UNBLOCK_RETRY_MS = 50;
const OVERLAY_INITIAL_KEY_CODE_I = 0x49;
const OVERLAY_INITIAL_KEY_CODE_R = 0x52;

export class OverlayProcess implements EditingTarget {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    readonly window: BrowserWindow;
    readonly overlay: OverlayInstance;
    readonly pid: number;

    private readonly hwnd: number;
    private readonly luid: GpuLuid;
    private readonly editingTarget: EditingTarget;
    private readonly runtime: WindowsOverlayRuntime;
    private surface: OverlaySurface | undefined;
    private readonly bridge: HostBridge;
    private input: OverlayInput | undefined = undefined;
    private resolveInputUnblocked: (() => void) | undefined = undefined;
    private inputCapturePromise: Promise<void> | undefined = undefined;
    private inputUnblockPromise: Promise<void> | undefined = undefined;
    private destroyPromise: Promise<void> | undefined = undefined;
    private destroying = false;
    private rendererLoaded = false;
    private destroyed = false;
    private destroyedEmitted = false;
    private nativeConnected = true;
    private editingValue = false;
    private visibleValue: boolean;
    private readonly pressedEditShortcutKeys = new Set<string>();
    private readonly consumedNativeEditShortcutKeys = new Set<string>();
    private readonly nativeEditShortcuts = new NativeEditShortcutState();
    private readonly handleNativeKeyboardInput = (
        nativeHwnd: number,
        input: KeyboardInput,
    ): void => {
        if (nativeHwnd !== this.hwnd) {
            return;
        }

        if (input.kind === "Key" && input.state === "Released") {
            if (input.key.code === OVERLAY_INITIAL_KEY_CODE_I) {
                this.consumedNativeEditShortcutKeys.delete("i");
            } else if (input.key.code === OVERLAY_INITIAL_KEY_CODE_R) {
                this.consumedNativeEditShortcutKeys.delete("r");
            }
        }

        const command = this.nativeEditShortcuts.handle(input);
        if (command === undefined || this.destroying || this.destroyed || !this.editingValue) {
            return;
        }

        this.consumedNativeEditShortcutKeys.add(command === "toggle-interaction" ? "i" : "r");
        this.sendEditShortcutCommand(command);
    };
    private readonly handleBeforeInputEvent = (event: ElectronEvent, input: Input): void => {
        const key = input.key.toLowerCase();
        const wasPressed = this.pressedEditShortcutKeys.has(key);
        if (input.type === "keyUp") {
            this.pressedEditShortcutKeys.delete(key);
            this.consumedNativeEditShortcutKeys.delete(key);
            return;
        }
        if (input.type === "keyDown" && (key === "i" || key === "r")) {
            this.pressedEditShortcutKeys.add(key);
        }
        if (this.destroying || this.destroyed || !this.editingValue) {
            return;
        }

        const consumedByNative = this.consumedNativeEditShortcutKeys.has(key);
        const exactChord =
            input.type === "keyDown" &&
            (consumedByNative ||
                (input.control &&
                    !input.alt &&
                    !input.meta &&
                    !input.shift &&
                    (key === "i" || key === "r")));
        if (!exactChord) {
            return;
        }
        event.preventDefault();

        const command = editShortcutCommand(input);
        if (consumedByNative || command === undefined || wasPressed) {
            return;
        }
        this.sendEditShortcutCommand(command);
    };

    private readonly handleSurfaceError = (cause: unknown): void => {
        console.error(`Overlay surface error for PID ${this.pid}`, cause);
        void this.destroy();
    };

    private constructor(
        pid: number,
        hwnd: number,
        overlay: OverlayInstance,
        window: BrowserWindow,
        luid: GpuLuid,
        surface: OverlaySurface | undefined,
        editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        runtime: WindowsOverlayRuntime,
    ) {
        this.pid = pid;
        this.hwnd = hwnd;
        this.overlay = overlay;
        this.window = window;
        this.luid = luid;
        this.editingTarget = editingTarget;
        this.runtime = runtime;
        this.surface = surface;
        this.visibleValue = surface !== undefined;
        this.bridge = new HostBridge(
            window.webContents,
            editingTarget,
            options,
            configStore,
            overlayRegistry,
        );
        window.webContents.on("before-input-event", this.handleBeforeInputEvent);
        overlay.event.on("keyboard_input", this.handleNativeKeyboardInput);
        overlay.event.on("resized", (resizedHwnd: number, width: number, height: number): void => {
            if (resizedHwnd === this.hwnd && !this.window.isDestroyed()) {
                this.window.setSize(width, height, false);
            }
        });
        overlay.event.on("input_blocking_ended", (inputHwnd: number): void => {
            if (inputHwnd === this.hwnd) {
                const requested = this.resolveInputUnblocked !== undefined;
                this.releaseEditingInput();
                this.finishInputUnblock();
                if (
                    !requested &&
                    !this.destroying &&
                    !this.destroyed &&
                    this.editingTarget.editing
                ) {
                    void this.editingTarget.setEditing(false).catch((cause: unknown) => {
                        console.error(
                            `Failed to reconcile ended input blocking for PID ${this.pid}`,
                            cause,
                        );
                    });
                }
            }
        });
        this.surface?.events.on("error", this.handleSurfaceError);
        window.on("closed", () => {
            void this.destroy();
        });

        this.bridge.start();
    }

    get editing(): boolean {
        return this.editingValue;
    }

    get visible(): boolean {
        return this.visibleValue;
    }

    async setVisible(visible: boolean): Promise<void> {
        if (this.destroying) {
            await this.destroyPromise;
            return;
        }
        if (this.destroyed || visible === this.visibleValue) {
            return;
        }

        if (visible) {
            const surface = this.runtime.surface.connect(
                { id: this.hwnd, overlay: this.overlay },
                this.luid,
                this.window.webContents,
            );
            surface.events.on("error", this.handleSurfaceError);
            this.surface = surface;
            this.visibleValue = true;
            return;
        }

        await this.setEditing(false);
        const { surface } = this;
        surface?.events.off("error", this.handleSurfaceError);
        this.surface = undefined;
        this.visibleValue = false;
        await surface?.disconnect();
    }

    async setEditing(editing: boolean): Promise<void> {
        if (this.destroying) {
            await this.destroyPromise;
            return;
        }
        const nextEditing = editing && this.visibleValue;
        if (this.destroyed || nextEditing === this.editingValue) {
            return;
        }

        await (nextEditing ? this.startBlockingInput() : this.stopBlockingInput());
        if (!this.destroying && !this.destroyed) {
            await this.bridge.send({
                editing: this.editingValue,
                type: "editing",
            });
        }
    }

    async toggleEditing(): Promise<void> {
        await this.setEditing(!this.editingValue);
    }

    async flushPendingSave(): Promise<void> {
        if (!this.destroying && !this.destroyed) {
            await this.bridge.requestSaveFlush();
        }
    }

    async reload(): Promise<void> {
        await this.flushPendingSave();
        if (!this.destroying && !this.destroyed) {
            this.window.webContents.reloadIgnoringCache();
        }
    }

    destroy(): Promise<void> {
        if (this.destroyPromise === undefined) {
            this.destroying = true;
            // Defer the body so the shared promise is installed before any
            // Synchronous disconnect callback can re-enter destroy().
            this.destroyPromise = Promise.resolve()
                .then(() => this.finishDestruction())
                .catch((cause: unknown) => {
                    console.error(`Failed to tear down overlay for PID ${this.pid}`, cause);
                });
        }
        return this.destroyPromise;
    }

    private handleOverlayError(cause: unknown): void {
        console.error(`Overlay IPC error for PID ${this.pid}`, cause);
        void this.destroy();
    }

    private handleOverlayDisconnected(): void {
        this.nativeConnected = false;
        void this.destroy();
    }

    private handleTargetDestroyed(): void {
        void this.destroy();
    }

    private startBlockingInput(): Promise<void> {
        if (this.inputCapturePromise === undefined) {
            const capture = this.finishStartBlockingInput();
            this.inputCapturePromise = capture;
            const clear = (): void => {
                if (this.inputCapturePromise === capture) {
                    this.inputCapturePromise = undefined;
                }
            };
            void capture.then(clear, clear);
        }
        return this.inputCapturePromise;
    }

    private async finishStartBlockingInput(): Promise<void> {
        this.nativeEditShortcuts.clear();
        this.consumedNativeEditShortcutKeys.clear();
        await withFatalDeadline(
            () => Promise.resolve(this.overlay.blockInput(this.hwnd, true)),
            INPUT_REQUEST_TIMEOUT_MS,
            `Native input capture timed out for PID ${this.pid}; terminating vosulay to release game input`,
        );

        if (this.destroying || this.destroyed) {
            await this.stopBlockingInput();
            return;
        }

        try {
            this.input = this.runtime.input.connect(
                { id: this.hwnd, overlay: this.overlay },
                this.window.webContents,
            );
            this.editingValue = true;
            this.window.focusOnWebView();
        } catch (error) {
            await this.stopBlockingInput();
            throw error;
        }
    }

    private stopBlockingInput(): Promise<void> {
        if (this.inputUnblockPromise === undefined) {
            const unblock = this.finishStopBlockingInput();
            this.inputUnblockPromise = unblock;
            const clear = (): void => {
                if (this.inputUnblockPromise === unblock) {
                    this.inputUnblockPromise = undefined;
                }
            };
            void unblock.then(clear, clear);
        }
        return this.inputUnblockPromise;
    }

    private async finishStopBlockingInput(): Promise<void> {
        let resolveUnblocked: (() => void) | undefined = undefined;
        let unblocked = false;
        const unblockedPromise = new Promise<void>((resolve) => {
            resolveUnblocked = () => {
                unblocked = true;
                resolve();
            };
        });
        this.resolveInputUnblocked = resolveUnblocked;

        const retryRelease = async (): Promise<void> => {
            await Promise.resolve(this.overlay.blockInput(this.hwnd, false));
            if (unblocked) {
                return;
            }

            await Promise.race([
                unblockedPromise,
                new Promise<void>((resolve) => {
                    setTimeout(resolve, INPUT_UNBLOCK_RETRY_MS);
                }),
            ]);

            if (unblocked) {
                return;
            }

            await retryRelease();
        };

        try {
            await withFatalDeadline(
                retryRelease,
                INPUT_REQUEST_TIMEOUT_MS,
                `Native input release timed out for PID ${this.pid}; terminating vosulay to release game input`,
            );
        } finally {
            if (this.resolveInputUnblocked === resolveUnblocked) {
                this.resolveInputUnblocked = undefined;
            }
        }
    }
    private finishInputUnblock(): void {
        const { resolveInputUnblocked } = this;
        this.resolveInputUnblocked = undefined;
        resolveInputUnblocked?.();
    }

    private releaseEditingInput(): void {
        try {
            this.input?.disconnect();
        } catch (error) {
            console.error(`Failed to disconnect overlay input for PID ${this.pid}`, error);
        }
        this.input = undefined;
        this.pressedEditShortcutKeys.clear();
        this.nativeEditShortcuts.clear();
        this.consumedNativeEditShortcutKeys.clear();
        this.editingValue = false;
        if (!this.window.isDestroyed()) {
            this.window.blurWebView();
            void this.bridge.send({ editing: false, type: "editing" });
        }
    }

    private sendEditShortcutCommand(command: EditShortcutCommand): void {
        void this.bridge.send({ type: command }).catch((cause: unknown) => {
            this.handleOverlayError(cause);
        });
    }

    private destroyNativeOverlay(): void {
        if (!this.nativeConnected) {
            return;
        }
        this.nativeConnected = false;
        try {
            this.overlay.destroy();
        } catch (cause) {
            console.error(`Failed to destroy native overlay for PID ${this.pid}`, cause);
            fatalProcessExit(
                `Native overlay teardown failed for PID ${this.pid}; terminating vosulay to release game input`,
            );
        }
    }

    private async finishDestruction(): Promise<void> {
        if (this.destroyed) {
            return;
        }
        this.overlay.event.off("keyboard_input", this.handleNativeKeyboardInput);
        if (!this.window.isDestroyed()) {
            const { webContents } = this.window;
            if (!webContents.isDestroyed()) {
                webContents.removeListener("before-input-event", this.handleBeforeInputEvent);
            }
        }
        const capture = this.inputCapturePromise;
        if (capture !== undefined) {
            try {
                await capture;
            } catch (error) {
                console.error(
                    `Failed to finish input capture before destroying PID ${this.pid}`,
                    error,
                );
            }
        }
        if (this.editingValue || this.inputUnblockPromise !== undefined) {
            try {
                await this.stopBlockingInput();
            } catch (error) {
                console.error(`Failed to release input before destroying PID ${this.pid}`, error);
                fatalProcessExit(
                    `Native input release failed for PID ${this.pid}; terminating vosulay to release game input`,
                );
            }
        }
        this.finishInputUnblock();

        if (this.rendererLoaded && !this.window.isDestroyed()) {
            try {
                await this.bridge.requestSaveFlush();
            } catch (error) {
                console.error(`Failed to save overlay state before closing PID ${this.pid}`, error);
            }
        }

        try {
            this.input?.disconnect();
        } catch (error) {
            console.error(`Failed to disconnect overlay input for PID ${this.pid}`, error);
        }
        this.input = undefined;
        try {
            this.bridge.dispose();
        } catch (error) {
            console.error(`Failed to dispose overlay bridge for PID ${this.pid}`, error);
        }
        const { surface } = this;
        this.surface = undefined;
        this.visibleValue = false;
        surface?.events.off("error", this.handleSurfaceError);
        try {
            await surface?.disconnect();
        } catch (error) {
            console.error(`Failed to disconnect overlay surface for PID ${this.pid}`, error);
        }
        this.destroyNativeOverlay();
        if (!this.window.isDestroyed()) {
            try {
                if (this.window.webContents.debugger.isAttached()) {
                    this.window.webContents.debugger.detach();
                }
            } catch (error) {
                console.error(`Failed to detach overlay debugger for PID ${this.pid}`, error);
            }
            try {
                this.window.destroy();
            } catch (error) {
                console.error(`Failed to destroy overlay window for PID ${this.pid}`, error);
            }
        }
        this.destroyed = true;
        if (!this.destroyedEmitted) {
            this.destroyedEmitted = true;
            this.events.emit("destroyed");
        }
    }

    static async initialize(
        pid: number,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        editingTarget: EditingTarget,
        visible: boolean,
    ): Promise<OverlayProcess> {
        const runtime = loadWindowsOverlayRuntime();
        const overlay = await runtime.overlay.attach(
            runtime.defaultDllDir().replaceAll("app.asar", "app.asar.unpacked"),
            pid,
            OVERLAY_ATTACH_TIMEOUT_MS,
        );

        let initializationComplete = false;
        let initializationError: unknown = undefined;
        let initializationFailed = false;
        let overlayConnected = true;
        let overlayProcess: OverlayProcess | undefined = undefined;
        let overlayWindow: BrowserWindow | undefined = undefined;
        let rejectInitialization: ((cause: unknown) => void) | undefined = undefined;
        let surface: OverlaySurface | undefined = undefined;
        let targetHwnd: number | undefined = undefined;

        const failInitialization = (cause: unknown): void => {
            if (!initializationFailed) {
                initializationFailed = true;
                initializationError = cause;
            }
            rejectInitialization?.(initializationError);
        };

        const onOverlayError = (cause: unknown): void => {
            if (initializationComplete) {
                overlayProcess?.handleOverlayError(cause);
                return;
            }
            failInitialization(cause);
            if (overlayProcess === undefined) {
                overlay.destroy();
            } else {
                void overlayProcess.destroy();
            }
        };

        const onOverlayDisconnected = (): void => {
            overlayConnected = false;
            if (overlayProcess !== undefined) {
                overlayProcess.handleOverlayDisconnected();
            }
            if (!initializationComplete) {
                failInitialization(new Error(`Overlay IPC disconnected for PID ${pid}`));
            }
        };

        const onTargetDestroyed = (destroyedHwnd: number): void => {
            if (targetHwnd === undefined || destroyedHwnd !== targetHwnd) {
                return;
            }
            if (initializationComplete) {
                overlayProcess?.handleTargetDestroyed();
                return;
            }
            failInitialization(new Error(`Overlay target window ${destroyedHwnd} was destroyed`));
            if (overlayProcess !== undefined) {
                void overlayProcess.destroy();
            }
        };

        // Asdf throws errors from its async event pump when no listener exists.
        // Keep these listeners installed continuously from attach through teardown.
        overlay.event.on("error", onOverlayError);
        overlay.event.on("disconnected", onOverlayDisconnected);
        overlay.event.on("destroyed", onTargetDestroyed);

        const throwIfInitializationFailed = (): void => {
            if (initializationFailed) {
                throw initializationError;
            }
        };

        const cleanupOverlayWindow = (): void => {
            const targetWindow = overlayWindow;
            if (targetWindow === undefined || targetWindow.isDestroyed()) {
                return;
            }
            try {
                if (targetWindow.webContents.debugger.isAttached()) {
                    targetWindow.webContents.debugger.detach();
                }
            } catch (detachError) {
                console.error(`Failed to detach overlay debugger for PID ${pid}`, detachError);
            }
            try {
                targetWindow.destroy();
            } catch (windowError) {
                console.error(`Failed to destroy overlay window for PID ${pid}`, windowError);
            }
        };

        try {
            const [hwnd, width, height, luid] = await new Promise<
                [number, number, number, GpuLuid]
            >((resolve, reject) => {
                const candidates = new Map<number, OverlayTarget>();
                let finished = false;
                const timeoutState: TimeoutState = {};

                const cleanup = (): void => {
                    if (finished) {
                        return;
                    }
                    finished = true;
                    if (timeoutState.timeout !== undefined) {
                        clearTimeout(timeoutState.timeout);
                    }
                    if (timeoutState.settleTimeout !== undefined) {
                        clearTimeout(timeoutState.settleTimeout);
                    }
                    overlay.event.off("added", onAdded);
                    overlay.event.off("resized", onCandidateResized);
                    overlay.event.off("destroyed", onCandidateDestroyed);
                    rejectInitialization = undefined;
                };

                const describeCandidates = (): string => {
                    const descriptions = [...candidates.values()].map(
                        (candidate) =>
                            `${candidate.hwnd}:${candidate.width}x${candidate.height}` +
                            `@${candidate.luid.high}:${candidate.luid.low}`,
                    );
                    return descriptions.length > 0 ? descriptions.join(", ") : "none";
                };

                const selectBestCandidate = (): void => {
                    const selected = chooseOverlayTarget(candidates.values());
                    if (selected === undefined) {
                        return;
                    }
                    targetHwnd = selected.hwnd;
                    const observed = describeCandidates();
                    cleanup();
                    console.log(
                        `Selected overlay window ${selected.hwnd} ` +
                            `(${selected.width}x${selected.height}) for PID ${pid}; ` +
                            `observed ${observed}`,
                    );
                    resolve([selected.hwnd, selected.width, selected.height, selected.luid]);
                };

                const scheduleSelection = (): void => {
                    const { settleTimeout } = timeoutState;
                    if (settleTimeout !== undefined) {
                        clearTimeout(settleTimeout);
                        delete timeoutState.settleTimeout;
                    }
                    if (chooseOverlayTarget(candidates.values()) !== undefined) {
                        timeoutState.settleTimeout = setTimeout(
                            selectBestCandidate,
                            OVERLAY_WINDOW_SETTLE_MS,
                        );
                    }
                };

                const onAdded = (
                    addedHwnd: number,
                    addedWidth: number,
                    addedHeight: number,
                    addedLuid: GpuLuid,
                ): void => {
                    candidates.set(addedHwnd, {
                        height: addedHeight,
                        hwnd: addedHwnd,
                        luid: addedLuid,
                        width: addedWidth,
                    });
                    console.log(
                        `Observed overlay window ${addedHwnd} ` +
                            `(${addedWidth}x${addedHeight}) for PID ${pid}`,
                    );
                    scheduleSelection();
                };

                const onCandidateResized = (
                    resizedHwnd: number,
                    resizedWidth: number,
                    resizedHeight: number,
                ): void => {
                    const candidate = candidates.get(resizedHwnd);
                    if (candidate === undefined) {
                        return;
                    }
                    candidate.width = resizedWidth;
                    candidate.height = resizedHeight;
                    console.log(
                        `Observed overlay window ${resizedHwnd} resize ` +
                            `to ${resizedWidth}x${resizedHeight} for PID ${pid}`,
                    );
                    scheduleSelection();
                };

                const onCandidateDestroyed = (destroyedHwnd: number): void => {
                    if (candidates.delete(destroyedHwnd)) {
                        console.log(
                            `Overlay window ${destroyedHwnd} was destroyed ` +
                                `before selection for PID ${pid}`,
                        );
                        scheduleSelection();
                    }
                };
                timeoutState.timeout = setTimeout(() => {
                    failInitialization(
                        new Error(
                            `Timed out waiting for a viable overlay window for PID ${pid}; ` +
                                `observed ${describeCandidates()}`,
                        ),
                    );
                }, OVERLAY_WINDOW_TIMEOUT_MS);
                rejectInitialization = (cause: unknown): void => {
                    cleanup();
                    reject(cause);
                };
                overlay.event.on("added", onAdded);
                overlay.event.on("resized", onCandidateResized);
                overlay.event.on("destroyed", onCandidateDestroyed);
                if (initializationFailed) {
                    rejectInitialization(initializationError);
                }
            });
            throwIfInitializationFailed();

            await overlay.setPosition(hwnd, runtime.length(0), runtime.length(0));
            throwIfInitializationFailed();
            await overlay.setAnchor(hwnd, runtime.length(0), runtime.length(0));
            throwIfInitializationFailed();
            await overlay.setMargin(
                hwnd,
                runtime.length(0),
                runtime.length(0),
                runtime.length(0),
                runtime.length(0),
            );
            throwIfInitializationFailed();

            const vosuOrigin = options.vosuUrl.origin;
            overlayWindow = new BrowserWindow({
                backgroundColor: "#00000000",
                frame: false,
                show: false,
                transparent: true,
                webPreferences: {
                    additionalArguments: [`--vosulay-vosu-url=${options.vosuUrl.href}`],
                    backgroundThrottling: false,
                    contextIsolation: true,
                    nodeIntegration: false,
                    offscreen: {
                        sharedTexturePixelFormat: "argb",
                        useSharedTexture: true,
                    },
                    preload: PRELOAD_PATH,
                    sandbox: true,
                    transparent: true,
                },
            });
            overlayWindow.setSize(width, height, false);
            await overlayWindow.loadURL(
                "data:text/html,<style>html,body{background:transparent}</style>",
            );
            throwIfInitializationFailed();

            overlayWindow.webContents.setWindowOpenHandler(() => ({
                action: "deny",
            }));
            overlayWindow.webContents.on("will-attach-webview", (event) => {
                event.preventDefault();
            });
            overlayWindow.webContents.on("will-navigate", (event) => {
                event.preventDefault();
            });

            overlayWindow.webContents.debugger.attach("1.3");
            await overlayWindow.webContents.debugger.sendCommand("Page.enable");
            throwIfInitializationFailed();
            await overlayWindow.webContents.debugger.sendCommand(
                "Page.addScriptToEvaluateOnNewDocument",
                {
                    source: counterMetadataScript(vosuOrigin),
                },
            );
            throwIfInitializationFailed();

            if (visible) {
                surface = runtime.surface.connect(
                    { id: hwnd, overlay },
                    luid,
                    overlayWindow.webContents,
                );
            }
            overlayProcess = new OverlayProcess(
                pid,
                hwnd,
                overlay,
                overlayWindow,
                luid,
                surface,
                editingTarget,
                options,
                configStore,
                overlayRegistry,
                runtime,
            );
            throwIfInitializationFailed();
            await overlayWindow.loadURL(MANAGER_URL);
            overlayWindow.webContents.navigationHistory.clear();
            overlayProcess.rendererLoaded = true;
            throwIfInitializationFailed();
            initializationComplete = true;
            return overlayProcess;
        } catch (error) {
            if (overlayProcess === undefined) {
                if (surface !== undefined) {
                    try {
                        await surface.disconnect();
                    } catch (disconnectError) {
                        console.error(
                            `Failed to disconnect overlay surface for PID ${pid}`,
                            disconnectError,
                        );
                    }
                }
                cleanupOverlayWindow();
                if (overlayConnected) {
                    overlayConnected = false;
                    try {
                        overlay.destroy();
                    } catch (destroyError) {
                        console.error(
                            `Failed to destroy native overlay for PID ${pid}`,
                            destroyError,
                        );
                    }
                }
            } else {
                await overlayProcess.destroy();
            }
            throw error;
        }
    }
}

function counterMetadataScript(vosuOrigin: string): string {
    return `(() => {
        try {
            if (window.location.origin !== ${JSON.stringify(vosuOrigin)} ||
                typeof window.COUNTER_PATH === 'string') return;
            const pathname = window.location.pathname.endsWith('/')
                ? window.location.pathname
                : window.location.pathname.slice(0, window.location.pathname.lastIndexOf('/') + 1);
            const counterPath = pathname.split('/').filter(Boolean)
                .map((segment) => decodeURIComponent(segment)).join('/');
            if (counterPath) window.COUNTER_PATH = counterPath;
        } catch (_) {}
    })();`;
}
