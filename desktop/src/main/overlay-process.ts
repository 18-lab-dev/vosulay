import { createRequire } from 'node:module';
import EventEmitter from 'node:events';
import path from 'node:path';

import {
    app,
    BrowserWindow,
    type Event as ElectronEvent,
    type Input,
    type WebContents
} from 'electron';

import type { RuntimeOptions } from './options';
import type { ConfigStore } from './config';
import type { LocalOverlayRegistry } from './local-overlays';
import type { EditingTarget } from './types';
import {
    type EditShortcutCommand,
    type NativeEditShortcutInput,
    NativeEditShortcutState,
    editShortcutCommand
} from './edit-shortcut';
import {
    chooseOverlayTarget,
    type GpuLuid,
    type OverlayTarget
} from './overlay-target';
import { HostBridge } from './host-bridge';
import {
    fatalProcessExit,
    withFatalDeadline
} from './fatal-deadline';

type OverlayEventListener<Args extends unknown[]> = (...args: Args) => void;

interface OverlayEvent {
    on(event: 'added', listener: OverlayEventListener<[number, number, number, GpuLuid]>): void;
    on(event: 'resized', listener: OverlayEventListener<[number, number, number]>): void;
    on(event: 'destroyed', listener: OverlayEventListener<[number]>): void;
    on(event: 'error', listener: OverlayEventListener<[unknown]>): void;
    on(event: 'disconnected', listener: OverlayEventListener<[]>): void;
    on(event: 'keyboard_input', listener: OverlayEventListener<[number, KeyboardInput]>): void;
    on(event: 'input_blocking_ended', listener: OverlayEventListener<[number]>): void;
    off(event: 'added', listener: OverlayEventListener<[number, number, number, GpuLuid]>): void;
    off(event: 'resized', listener: OverlayEventListener<[number, number, number]>): void;
    off(event: 'destroyed', listener: OverlayEventListener<[number]>): void;
    off(event: 'error', listener: OverlayEventListener<[unknown]>): void;
    off(event: 'disconnected', listener: OverlayEventListener<[]>): void;
    off(event: 'keyboard_input', listener: OverlayEventListener<[number, KeyboardInput]>): void;
    off(event: 'input_blocking_ended', listener: OverlayEventListener<[number]>): void;
}

interface OverlayInstance {
    readonly event: OverlayEvent;
    blockInput(hwnd: number, blocked: boolean): Promise<void> | void;
    destroy(): void;
    setPosition(hwnd: number, x: unknown, y: unknown): Promise<void> | void;
    setAnchor(hwnd: number, x: unknown, y: unknown): Promise<void> | void;
    setMargin(
        hwnd: number,
        top: unknown,
        right: unknown,
        bottom: unknown,
        left: unknown
    ): Promise<void> | void;
}

interface OverlayModule {
    attach(
        dllDirectory: string,
        pid: number,
        timeoutMs: number
    ): Promise<OverlayInstance>;
}

interface OverlayInput {
    disconnect(): void;
}

interface OverlayInputModule {
    connect(
        target: { readonly id: number; readonly overlay: OverlayInstance },
        webContents: WebContents
    ): OverlayInput;
}

interface SurfaceEvents {
    on(event: 'error', listener: OverlayEventListener<[unknown]>): void;
    off(event: 'error', listener: OverlayEventListener<[unknown]>): void;
}

interface OverlaySurface {
    readonly events: SurfaceEvents;
    disconnect(): Promise<void> | void;
}

interface OverlaySurfaceModule {
    connect(
        target: { readonly id: number; readonly overlay: OverlayInstance },
        luid: GpuLuid,
        webContents: WebContents
    ): OverlaySurface;
}

interface WindowsOverlayRuntime {
    readonly overlay: OverlayModule;
    readonly defaultDllDir: () => string;
    readonly length: (value: number) => unknown;
    readonly input: OverlayInputModule;
    readonly surface: OverlaySurfaceModule;
}

interface KeyboardInput extends NativeEditShortcutInput {
    readonly key: { readonly code: number };
    readonly state: string;
}

const loadWindowsOverlayRuntime = (): WindowsOverlayRuntime => {
    const require = createRequire(import.meta.url);
    const core = require('@asdf-overlay/core') as {
        readonly Overlay: OverlayModule;
        readonly defaultDllDir: () => string;
        readonly length: (value: number) => unknown;
    };
    const input = require('@asdf-overlay/electron/input') as {
        readonly ElectronOverlayInput: OverlayInputModule;
    };
    const surface = require('@asdf-overlay/electron/surface') as {
        readonly ElectronOverlaySurface: OverlaySurfaceModule;
    };
    return {
        overlay: core.Overlay,
        defaultDllDir: core.defaultDllDir,
        length: core.length,
        input: input.ElectronOverlayInput,
        surface: surface.ElectronOverlaySurface
    };
};
const MANAGER_URL = 'vosulay://manager/index.html';
const PRELOAD_PATH = path.join(app.getAppPath(), 'out/preload/index.js');
const OVERLAY_ATTACH_TIMEOUT_MS = 5_000;
const OVERLAY_WINDOW_TIMEOUT_MS = 30_000;
const OVERLAY_WINDOW_SETTLE_MS = 250;
const INPUT_REQUEST_TIMEOUT_MS = 5_000;
const INPUT_UNBLOCK_RETRY_MS = 50;


export class OverlayProcess implements EditingTarget {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    readonly window: BrowserWindow;
    readonly overlay: OverlayInstance;
    readonly pid: number;

    private readonly hwnd: number;
    private readonly luid: GpuLuid;
    private surface: OverlaySurface | null;
    private readonly bridge: HostBridge;
    private input: OverlayInput | null = null;
    private resolveInputUnblocked: (() => void) | null = null;
    private inputCapturePromise: Promise<void> | null = null;
    private inputUnblockPromise: Promise<void> | null = null;
    private destroyPromise: Promise<void> | null = null;
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
        input: KeyboardInput
    ): void => {
        if (nativeHwnd !== this.hwnd) {
            return;
        }

        if (input.kind === 'Key' && input.state === 'Released') {
            if (input.key.code === 0x49) {
                this.consumedNativeEditShortcutKeys.delete('i');
            } else if (input.key.code === 0x52) {
                this.consumedNativeEditShortcutKeys.delete('r');
            }
        }

        const command = this.nativeEditShortcuts.handle(input);
        if (
            command === null
            || this.destroying
            || this.destroyed
            || !this.editingValue
        ) {
            return;
        }

        this.consumedNativeEditShortcutKeys.add(
            command === 'toggle-interaction' ? 'i' : 'r'
        );
        this.sendEditShortcutCommand(command);
    };
    private readonly handleBeforeInputEvent = (
        event: ElectronEvent,
        input: Input
    ): void => {
        const key = input.key.toLowerCase();
        const wasPressed = this.pressedEditShortcutKeys.has(key);
        if (input.type === 'keyUp') {
            this.pressedEditShortcutKeys.delete(key);
            this.consumedNativeEditShortcutKeys.delete(key);
            return;
        }
        if (input.type === 'keyDown' && (key === 'i' || key === 'r')) {
            this.pressedEditShortcutKeys.add(key);
        }
        if (this.destroying || this.destroyed || !this.editingValue) {
            return;
        }

        const consumedByNative =
            this.consumedNativeEditShortcutKeys.has(key);
        const exactChord = input.type === 'keyDown'
            && (
                consumedByNative
                || (
                    input.control
                    && !input.alt
                    && !input.meta
                    && !input.shift
                    && (key === 'i' || key === 'r')
                )
            );
        if (!exactChord) {
            return;
        }
        event.preventDefault();

        const command = editShortcutCommand(input);
        if (consumedByNative || command === null || wasPressed) {
            return;
        }
        this.sendEditShortcutCommand(command);
    };

    private readonly handleSurfaceError = (error: unknown): void => {
        console.error(`Overlay surface error for PID ${this.pid}`, error);
        void this.destroy();
    };

    private constructor(
        pid: number,
        hwnd: number,
        overlay: OverlayInstance,
        window: BrowserWindow,
        luid: GpuLuid,
        surface: OverlaySurface | null,
        private readonly editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        private readonly runtime: WindowsOverlayRuntime
    ) {
        this.pid = pid;
        this.hwnd = hwnd;
        this.overlay = overlay;
        this.window = window;
        this.luid = luid;
        this.surface = surface;
        this.visibleValue = surface !== null;
        this.bridge = new HostBridge(
            window.webContents,
            editingTarget,
            options,
            configStore,
            overlayRegistry
        );
        window.webContents.on(
            'before-input-event',
            this.handleBeforeInputEvent
        );
        overlay.event.on(
            'keyboard_input',
            this.handleNativeKeyboardInput
        );
        overlay.event.on(
            'resized',
            (
                resizedHwnd: number,
                width: number,
                height: number
            ): void => {
                if (resizedHwnd === this.hwnd && !this.window.isDestroyed()) {
                    this.window.setSize(width, height, false);
                }
            }
        );
        overlay.event.on(
            'input_blocking_ended',
            (inputHwnd: number): void => {
            if (inputHwnd === this.hwnd) {
                const requested = this.resolveInputUnblocked !== null;
                this.releaseEditingInput();
                this.finishInputUnblock();
                if (
                    !requested &&
                    !this.destroying &&
                    !this.destroyed &&
                    this.editingTarget.editing
                ) {
                    void this.editingTarget.setEditing(false).catch((error: unknown) => {
                        console.error(
                            `Failed to reconcile ended input blocking for PID ${this.pid}`,
                            error
                        );
                    });
                }
            }
        });
        this.surface?.events.on('error', this.handleSurfaceError);
        window.on('closed', () => {
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
                this.window.webContents
            );
            surface.events.on('error', this.handleSurfaceError);
            this.surface = surface;
            this.visibleValue = true;
            return;
        }

        await this.setEditing(false);
        const surface = this.surface;
        surface?.events.off('error', this.handleSurfaceError);
        this.surface = null;
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

        if (nextEditing) {
            await this.startBlockingInput();
        } else {
            await this.stopBlockingInput();
        }
        if (!this.destroying && !this.destroyed) {
            await this.bridge.send({
                type: 'editing',
                editing: this.editingValue
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
        if (this.destroyPromise === null) {
            this.destroying = true;
            // Defer the body so the shared promise is installed before any
            // synchronous disconnect callback can re-enter destroy().
            this.destroyPromise = Promise.resolve()
                .then(() => this.finishDestruction())
                .catch((error: unknown) => {
                    console.error(
                        `Failed to tear down overlay for PID ${this.pid}`,
                        error
                    );
                });
        }
        return this.destroyPromise;
    }

    private handleOverlayError(error: unknown): void {
        console.error(`Overlay IPC error for PID ${this.pid}`, error);
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
        if (this.inputCapturePromise === null) {
            const capture = this.finishStartBlockingInput();
            this.inputCapturePromise = capture;
            const clear = (): void => {
                if (this.inputCapturePromise === capture) {
                    this.inputCapturePromise = null;
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
            () => Promise.resolve(
                this.overlay.blockInput(this.hwnd, true)
            ),
            INPUT_REQUEST_TIMEOUT_MS,
            `Native input capture timed out for PID ${this.pid}; terminating vosulay to release game input`
        );

        if (this.destroying || this.destroyed) {
            await this.stopBlockingInput();
            return;
        }

        try {
            this.input = this.runtime.input.connect(
                { id: this.hwnd, overlay: this.overlay },
                this.window.webContents
            );
            this.editingValue = true;
            this.window.focusOnWebView();
        } catch (error) {
            await this.stopBlockingInput();
            throw error;
        }
    }

    private stopBlockingInput(): Promise<void> {
        if (this.inputUnblockPromise === null) {
            const unblock = this.finishStopBlockingInput();
            this.inputUnblockPromise = unblock;
            const clear = (): void => {
                if (this.inputUnblockPromise === unblock) {
                    this.inputUnblockPromise = null;
                }
            };
            void unblock.then(clear, clear);
        }
        return this.inputUnblockPromise;
    }

    private async finishStopBlockingInput(): Promise<void> {
        let unblocked = false;
        let resolveUnblocked!: () => void;
        const unblockedPromise = new Promise<void>((resolve) => {
            resolveUnblocked = () => {
                unblocked = true;
                resolve();
            };
        });
        this.resolveInputUnblocked = resolveUnblocked;

        try {
            await withFatalDeadline(
                async () => {
                    while (!unblocked) {
                        await Promise.resolve(
                            this.overlay.blockInput(this.hwnd, false)
                        );
                        if (!unblocked) {
                            await Promise.race([
                                unblockedPromise,
                                new Promise<void>((resolve) => {
                                    setTimeout(
                                        resolve,
                                        INPUT_UNBLOCK_RETRY_MS
                                    );
                                })
                            ]);
                        }
                    }
                },
                INPUT_REQUEST_TIMEOUT_MS,
                `Native input release timed out for PID ${this.pid}; terminating vosulay to release game input`
            );
        } finally {
            if (this.resolveInputUnblocked === resolveUnblocked) {
                this.resolveInputUnblocked = null;
            }
        }
    }

    private finishInputUnblock(): void {
        const resolve = this.resolveInputUnblocked;
        this.resolveInputUnblocked = null;
        resolve?.();
    }

    private releaseEditingInput(): void {
        try {
            this.input?.disconnect();
        } catch (error) {
            console.error(`Failed to disconnect overlay input for PID ${this.pid}`, error);
        }
        this.input = null;
        this.pressedEditShortcutKeys.clear();
        this.nativeEditShortcuts.clear();
        this.consumedNativeEditShortcutKeys.clear();
        this.editingValue = false;
        if (!this.window.isDestroyed()) {
            this.window.blurWebView();
            void this.bridge.send({ type: 'editing', editing: false });
        }
    }

    private sendEditShortcutCommand(command: EditShortcutCommand): void {
        void this.bridge.send({ type: command }).catch((error: unknown) => {
            this.handleOverlayError(error);
        });
    }

    private destroyNativeOverlay(): void {
        if (!this.nativeConnected) {
            return;
        }
        this.nativeConnected = false;
        try {
            this.overlay.destroy();
        } catch (error) {
            console.error(
                `Failed to destroy native overlay for PID ${this.pid}`,
                error
            );
            fatalProcessExit(
                `Native overlay teardown failed for PID ${this.pid}; terminating vosulay to release game input`
            );
        }
    }

    private async finishDestruction(): Promise<void> {
        if (this.destroyed) {
            return;
        }
        this.overlay.event.off(
            'keyboard_input',
            this.handleNativeKeyboardInput
        );
        if (!this.window.isDestroyed()) {
            const webContents = this.window.webContents;
            if (!webContents.isDestroyed()) {
                webContents.removeListener(
                    'before-input-event',
                    this.handleBeforeInputEvent
                );
            }
        }
        const capture = this.inputCapturePromise;
        if (capture !== null) {
            try {
                await capture;
            } catch (error) {
                console.error(
                    `Failed to finish input capture before destroying PID ${this.pid}`,
                    error
                );
            }
        }
        if (this.editingValue || this.inputUnblockPromise !== null) {
            try {
                await this.stopBlockingInput();
            } catch (error) {
                console.error(
                    `Failed to release input before destroying PID ${this.pid}`,
                    error
                );
                fatalProcessExit(
                    `Native input release failed for PID ${this.pid}; terminating vosulay to release game input`
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
        this.input = null;
        try {
            this.bridge.dispose();
        } catch (error) {
            console.error(`Failed to dispose overlay bridge for PID ${this.pid}`, error);
        }
        const surface = this.surface;
        this.surface = null;
        this.visibleValue = false;
        surface?.events.off('error', this.handleSurfaceError);
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
            this.events.emit('destroyed');
        }
    }

    static async initialize(
        pid: number,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        editingTarget: EditingTarget,
        visible: boolean
    ): Promise<OverlayProcess> {
        const runtime = loadWindowsOverlayRuntime();
        const overlay = await runtime.overlay.attach(
            runtime.defaultDllDir().replaceAll('app.asar', 'app.asar.unpacked'),
            pid,
            OVERLAY_ATTACH_TIMEOUT_MS
        );
        let overlayWindow: BrowserWindow | null = null;
        let surface: OverlaySurface | null = null;
        let overlayProcess: OverlayProcess | null = null;
        let targetHwnd: number | null = null;
        let initializationError: unknown = null;
        let initializationFailed = false;
        let overlayConnected = true;
        let rejectInitialization: ((reason: unknown) => void) | null = null;
        let initializationComplete = false;

        const failInitialization = (error: unknown): void => {
            if (!initializationFailed) {
                initializationFailed = true;
                initializationError = error;
            }
            rejectInitialization?.(initializationError);
        };
        const onOverlayError = (error: unknown): void => {
            if (initializationComplete) {
                overlayProcess?.handleOverlayError(error);
                return;
            }
            failInitialization(error);
            if (overlayProcess !== null) {
                void overlayProcess.destroy();
            } else {
                overlay.destroy();
            }
        };
        const onOverlayDisconnected = (): void => {
            overlayConnected = false;
            if (overlayProcess !== null) {
                overlayProcess.handleOverlayDisconnected();
            }
            if (!initializationComplete) {
                failInitialization(
                    new Error(`Overlay IPC disconnected for PID ${pid}`)
                );
            }
        };
        const onTargetDestroyed = (destroyedHwnd: number): void => {
            if (targetHwnd === null || destroyedHwnd !== targetHwnd) {
                return;
            }
            if (initializationComplete) {
                overlayProcess?.handleTargetDestroyed();
                return;
            }
            failInitialization(
                new Error(`Overlay target window ${destroyedHwnd} was destroyed`)
            );
            if (overlayProcess !== null) {
                void overlayProcess.destroy();
            }
        };

        // asdf throws errors from its async event pump when no listener exists.
        // Keep these listeners installed continuously from attach through teardown.
        overlay.event.on('error', onOverlayError);
        overlay.event.on('disconnected', onOverlayDisconnected);
        overlay.event.on('destroyed', onTargetDestroyed);

        const throwIfInitializationFailed = (): void => {
            if (initializationFailed) {
                throw initializationError;
            }
        };

        try {
            const [hwnd, width, height, luid] = await new Promise<
                [number, number, number, GpuLuid]
            >((resolve, reject) => {
                const candidates = new Map<number, OverlayTarget>();
                let timeout: NodeJS.Timeout | null = null;
                let settleTimeout: NodeJS.Timeout | null = null;
                let finished = false;

                const describeCandidates = (): string => {
                    const descriptions = [...candidates.values()].map((candidate) => (
                        `${candidate.hwnd}:${candidate.width}x${candidate.height}`
                        + `@${candidate.luid.high}:${candidate.luid.low}`
                    ));
                    return descriptions.length > 0 ? descriptions.join(', ') : 'none';
                };
                const cleanup = (): void => {
                    if (finished) {
                        return;
                    }
                    finished = true;
                    if (timeout !== null) {
                        clearTimeout(timeout);
                    }
                    if (settleTimeout !== null) {
                        clearTimeout(settleTimeout);
                    }
                    overlay.event.off('added', onAdded);
                    overlay.event.off('resized', onCandidateResized);
                    overlay.event.off('destroyed', onCandidateDestroyed);
                    rejectInitialization = null;
                };
                const selectBestCandidate = (): void => {
                    const selected = chooseOverlayTarget(candidates.values());
                    if (selected === null) {
                        return;
                    }
                    targetHwnd = selected.hwnd;
                    const observed = describeCandidates();
                    cleanup();
                    console.log(
                        `Selected overlay window ${selected.hwnd} `
                        + `(${selected.width}x${selected.height}) for PID ${pid}; `
                        + `observed ${observed}`
                    );
                    resolve([
                        selected.hwnd,
                        selected.width,
                        selected.height,
                        selected.luid
                    ]);
                };
                const scheduleSelection = (): void => {
                    if (settleTimeout !== null) {
                        clearTimeout(settleTimeout);
                        settleTimeout = null;
                    }
                    if (chooseOverlayTarget(candidates.values()) !== null) {
                        settleTimeout = setTimeout(
                            selectBestCandidate,
                            OVERLAY_WINDOW_SETTLE_MS
                        );
                    }
                };
                const onAdded = (
                    addedHwnd: number,
                    addedWidth: number,
                    addedHeight: number,
                    addedLuid: GpuLuid
                ): void => {
                    candidates.set(addedHwnd, {
                        hwnd: addedHwnd,
                        width: addedWidth,
                        height: addedHeight,
                        luid: addedLuid
                    });
                    console.log(
                        `Observed overlay window ${addedHwnd} `
                        + `(${addedWidth}x${addedHeight}) for PID ${pid}`
                    );
                    scheduleSelection();
                };
                const onCandidateResized = (
                    resizedHwnd: number,
                    resizedWidth: number,
                    resizedHeight: number
                ): void => {
                    const candidate = candidates.get(resizedHwnd);
                    if (candidate === undefined) {
                        return;
                    }
                    candidate.width = resizedWidth;
                    candidate.height = resizedHeight;
                    console.log(
                        `Observed overlay window ${resizedHwnd} resize `
                        + `to ${resizedWidth}x${resizedHeight} for PID ${pid}`
                    );
                    scheduleSelection();
                };
                const onCandidateDestroyed = (destroyedHwnd: number): void => {
                    if (candidates.delete(destroyedHwnd)) {
                        console.log(
                            `Overlay window ${destroyedHwnd} was destroyed `
                            + `before selection for PID ${pid}`
                        );
                        scheduleSelection();
                    }
                };

                timeout = setTimeout(() => {
                    failInitialization(
                        new Error(
                            `Timed out waiting for a viable overlay window for PID ${pid}; `
                            + `observed ${describeCandidates()}`
                        )
                    );
                }, OVERLAY_WINDOW_TIMEOUT_MS);
                rejectInitialization = (reason) => {
                    cleanup();
                    reject(reason);
                };
                overlay.event.on('added', onAdded);
                overlay.event.on('resized', onCandidateResized);
                overlay.event.on('destroyed', onCandidateDestroyed);
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
                runtime.length(0)
            );
            throwIfInitializationFailed();

            const vosuOrigin = options.vosuUrl.origin;
            overlayWindow = new BrowserWindow({
                show: false,
                frame: false,
                transparent: true,
                backgroundColor: '#00000000',
                webPreferences: {
                    preload: PRELOAD_PATH,
                    contextIsolation: true,
                    sandbox: true,
                    nodeIntegration: false,
                    offscreen: {
                        useSharedTexture: true,
                        sharedTexturePixelFormat: 'argb'
                    },
                    transparent: true,
                    backgroundThrottling: false,
                    additionalArguments: [
                        `--vosulay-vosu-url=${options.vosuUrl.href}`
                    ]
                }
            });
            overlayWindow.setSize(width, height, false);
            await overlayWindow.loadURL(
                'data:text/html,<style>html,body{background:transparent}</style>'
            );
            throwIfInitializationFailed();
            overlayWindow.webContents.setWindowOpenHandler(() => ({
                action: 'deny'
            }));
            overlayWindow.webContents.on('will-attach-webview', (event) => {
                event.preventDefault();
            });
            overlayWindow.webContents.on('will-navigate', (event) => {
                event.preventDefault();
            });

            overlayWindow.webContents.debugger.attach('1.3');
            await overlayWindow.webContents.debugger.sendCommand('Page.enable');
            throwIfInitializationFailed();
            await overlayWindow.webContents.debugger.sendCommand(
                'Page.addScriptToEvaluateOnNewDocument',
                {
                    source: counterMetadataScript(vosuOrigin)
                }
            );
            throwIfInitializationFailed();

            if (visible) {
                surface = runtime.surface.connect(
                    { id: hwnd, overlay },
                    luid,
                    overlayWindow.webContents
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
                runtime
            );
            throwIfInitializationFailed();
            await overlayWindow.loadURL(MANAGER_URL);
            overlayWindow.webContents.navigationHistory.clear();
            overlayProcess.rendererLoaded = true;
            throwIfInitializationFailed();
            initializationComplete = true;
            return overlayProcess;
        } catch (error) {
            if (overlayProcess !== null) {
                await overlayProcess.destroy();
            } else {
                if (surface !== null) {
                    try {
                        await surface.disconnect();
                    } catch (disconnectError) {
                        console.error(
                            `Failed to disconnect overlay surface for PID ${pid}`,
                            disconnectError
                        );
                    }
                }
                if (
                    overlayWindow !== null &&
                    !overlayWindow.isDestroyed()
                ) {
                    try {
                        if (overlayWindow.webContents.debugger.isAttached()) {
                            overlayWindow.webContents.debugger.detach();
                        }
                    } catch (detachError) {
                        console.error(
                            `Failed to detach overlay debugger for PID ${pid}`,
                            detachError
                        );
                    }
                    try {
                        overlayWindow.destroy();
                    } catch (windowError) {
                        console.error(
                            `Failed to destroy overlay window for PID ${pid}`,
                            windowError
                        );
                    }
                }
                if (overlayConnected) {
                    overlayConnected = false;
                    try {
                        overlay.destroy();
                    } catch (destroyError) {
                        console.error(
                            `Failed to destroy native overlay for PID ${pid}`,
                            destroyError
                        );
                    }
                }
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
