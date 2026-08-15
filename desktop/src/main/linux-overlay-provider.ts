import { createRequire } from "node:module";
import path from "node:path";
import EventEmitter from "node:events";
import type { ConfigStore } from "./config";
import { LinuxOverlayProcess } from "./linux-overlay-process";
import {
    OverlayManager,
    discoverTargetPids,
    type ManagedOverlayProcess,
    type OverlayManagerDependencies,
} from "./overlay-manager";
import { NoopForegroundTracker, type ForegroundTracker } from "./foreground-tracker";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import type { EditingTarget } from "./types";

export interface NativeSharedTexturePlane {
    fd: number;
    stride: number;
    offset: number;
    size: number;
}

export interface NativeSharedTextureDescriptor {
    targetId: number;
    sequence: number;
    width: number;
    height: number;
    fourcc: "AR24" | "AB24";
    modifier: string;
    planes: NativeSharedTexturePlane[];
}

export class NativeSharedTexture {
    static fromElectron(
        targetId: number,
        sequence: number,
        textureInfo: Electron.TextureInfo,
        _dirtyRect: Electron.Rectangle,
    ): NativeSharedTextureDescriptor | null {
        const pixmap = textureInfo.handle.nativePixmap;
        if (textureInfo.widgetType !== "frame" || pixmap === undefined
            || textureInfo.pixelFormat === "rgbaf16"
            || textureInfo.codedSize.width <= 0 || textureInfo.codedSize.height <= 0
            || pixmap.planes.length === 0 || pixmap.planes.length > 4) return null;
        return {
            targetId,
            sequence,
            width: textureInfo.codedSize.width,
            height: textureInfo.codedSize.height,
            fourcc: textureInfo.pixelFormat === "bgra" ? "AR24" : "AB24",
            modifier: pixmap.modifier,
            planes: pixmap.planes.map(({ fd, stride, offset, size }) => ({
                fd,
                stride,
                offset,
                size,
            })),
        };
    }
}

export interface NativeBitmapDescriptor {
    targetId: number;
    width: number;
    height: number;
    stride: number;
    format?: "BGRA" | "RGBA";
}

export interface NativeOverlaySurface {
    updateSharedTexture?(texture: unknown): Promise<void> | void;
    updateDmabuf(texture: NativeSharedTextureDescriptor): Promise<void> | void;
    updateBitmap?(descriptor: NativeBitmapDescriptor, bitmap: Buffer): Promise<void> | void;
    clear?(targetId: number): Promise<void> | void;
}

export interface NativeOverlayHandle {
    readonly pid?: number;
    supportsDmabuf?(): boolean;
    supportsBgraShm?(): boolean;
    readonly window?: ManagedOverlayProcess["window"];
    readonly events?: {
        on?(event: string, listener: (...args: unknown[]) => void): unknown;
        off?(event: string, listener: (...args: unknown[]) => void): unknown;
    };
    on?(event: string, listener: (...args: unknown[]) => void): unknown;
    off?(event: string, listener: (...args: unknown[]) => void): unknown;
    createSurface?(): Promise<NativeOverlaySurface> | NativeOverlaySurface;
    destroy(): Promise<void> | void;
    setVisible?(visible: boolean): Promise<void> | void;
    setEditing?(editing: boolean): Promise<void> | void;
    captureInput?(capture: boolean): Promise<void> | void;
    flushPendingSave?(): Promise<void> | void;
    reload?(): Promise<void> | void;
}

export interface NativeOverlayAddon {
    attach(
        pid: number,
        options?: Record<string, unknown>,
    ): Promise<NativeOverlayHandle> | NativeOverlayHandle;
    launchVulkan?(
        options: {
            executable: string;
            args: string[];
            layerDirectory: string;
            agentLibraryPath?: string;
            timeout?: number;
        },
    ): Promise<NativeOverlayHandle> | NativeOverlayHandle;
}

export interface LinuxOverlayProviderDependencies extends OverlayManagerDependencies {
    native?: NativeOverlayAddon;
    loadNative?: () => NativeOverlayAddon;
    foregroundTracker?: ForegroundTracker;
}

class LinuxPidTracker implements ForegroundTracker {
    private callback: ((pid: number | null) => void) | null = null;

    constructor(private readonly initialPid: number | null) {}

    start(onChanged: (pid: number | null) => void): number | null {
        this.callback = onChanged;
        return this.initialPid;
    }

    stop(): void {
        this.callback = null;
    }
}

/// Chooses which discovered process the overlay follows when there is no
/// window-manager foreground signal. Linux has no equivalent of the Windows
/// active-window API here, so selection must be deterministic instead of
/// arbitrary: the current choice is kept while it is still running, and
/// otherwise the lowest PID wins, which is the earliest-started match rather
/// than a helper the game spawned later.
export function selectCandidatePid(
    candidates: ReadonlySet<number>,
    current: number | null,
): number | null {
    if (current !== null && candidates.has(current)) return current;
    let selected: number | null = null;
    for (const pid of candidates) {
        if (selected === null || pid < selected) selected = pid;
    }
    return selected;
}

/// Foreground tracker fed by process discovery rather than by a compositor.
export class LinuxCandidateTracker implements ForegroundTracker {
    private callback: ((pid: number | null) => void) | null = null;
    private current: number | null = null;

    start(onChanged: (pid: number | null) => void): number | null {
        this.callback = onChanged;
        return this.current;
    }

    stop(): void {
        this.callback = null;
    }

    /// Called with each discovery result so the manager sees a foreground PID
    /// on the default path, where no explicit `--pid` was supplied.
    observe(candidates: ReadonlySet<number>): void {
        const next = selectCandidatePid(candidates, this.current);
        if (next === this.current) return;
        this.current = next;
        this.callback?.(next);
    }
}

class NativeOverlayProcess implements ManagedOverlayProcess {
    readonly events = new EventEmitter<{ destroyed: [] }>();
    private destroyed = false;
    private destroyedEmitted = false;

    constructor(
        readonly pid: number,
        private readonly native: NativeOverlayHandle,
    ) {
        const onDestroyed = (): void => {
            this.emitDestroyed();
        };
        native.events?.on?.("destroyed", onDestroyed);
        native.events?.on?.("disconnect", onDestroyed);
        native.on?.("disconnect", onDestroyed);
    }

    async destroy(): Promise<void> {
        if (this.destroyed) return;
        this.destroyed = true;
        await this.native.destroy();
        this.emitDestroyed();
    }

    async setEditing(editing: boolean): Promise<void> {
        if (this.native.setEditing !== undefined) {
            await this.native.setEditing(editing);
            return;
        }
        await this.native.captureInput?.(editing);
    }

    async setVisible(visible: boolean): Promise<void> {
        await this.native.setVisible?.(visible);
    }

    async flushPendingSave(): Promise<void> {
        await this.native.flushPendingSave?.();
    }

    async reload(): Promise<void> {
        await this.native.reload?.();
    }

    private emitDestroyed(): void {
        if (this.destroyedEmitted) return;
        this.destroyed = true;
        this.destroyedEmitted = true;
        this.events.emit("destroyed");
    }
}
export function nativeResourceRoot(): string | undefined {
    if (process.env.VOSULAY_NATIVE_ROOT !== undefined) {
        return process.env.VOSULAY_NATIVE_ROOT;
    }
    return "resourcesPath" in process && !process.defaultApp
        ? path.join(process.resourcesPath, "native")
        : undefined;
}

function nativeAttachOptions(options: RuntimeOptions): Record<string, unknown> {
    const root = nativeResourceRoot();
    if (root === undefined) {
        return { vosuUrl: options.vosuUrl.toString() };
    }
    return {
        vosuUrl: options.vosuUrl.toString(),
        attachToolPath: path.join(root, "dotnet", "Vosulay.AttachProfiler"),
        profilerLibraryPath: path.join(root, "liboverlay_dotnet_bootstrap.so"),
        agentLibraryPath: path.join(root, "liboverlay_agent_gl.so"),
    };
}

export function defaultNativeLoader(): NativeOverlayAddon {
    const root = nativeResourceRoot();
    const moduleName = process.env.VOSULAY_NATIVE_ADDON
        ?? (root === undefined
            ? "@vosu/overlay-node"
            : path.join(root, "overlay-node.linux-x64-gnu.node"));
    const loaded: unknown = createRequire(import.meta.url)(moduleName);
    if (!loaded || typeof loaded !== "object" || !("attach" in loaded)
        || typeof loaded.attach !== "function") {
        throw new Error(`Native overlay addon ${moduleName} does not expose attach(pid, options)`);
    }
    return loaded as NativeOverlayAddon;
}

export async function createLinuxOverlayProvider(
    options: RuntimeOptions,
    configStore: ConfigStore,
    overlayRegistry: LocalOverlayRegistry,
    dependencies: LinuxOverlayProviderDependencies = {},
): Promise<LinuxOverlayProvider> {
    if (options.vulkanLaunch === undefined) {
        return new LinuxOverlayProvider(options, configStore, overlayRegistry, dependencies);
    }
    if (options.pids.length > 0) {
        throw new Error("--launch-vulkan cannot be combined with --pid");
    }
    const root = nativeResourceRoot();
    if (root === undefined) {
        throw new Error(
            "--launch-vulkan requires packaged native resources or VOSULAY_NATIVE_ROOT",
        );
    }
    const addon = dependencies.native ?? (dependencies.loadNative ?? defaultNativeLoader)();
    if (addon.launchVulkan === undefined) {
        throw new Error("Native overlay addon does not expose launchVulkan(options)");
    }
    // Exactly one backend per launch. Preloading the GL agent alongside the
    // Vulkan layer pointed both at the same one-shot socket and cookie, so
    // whichever authenticated first claimed the session and the other backend
    // was left without an overlay.
    const handle = await addon.launchVulkan({
        executable: options.vulkanLaunch.executable,
        args: options.vulkanLaunch.args,
        layerDirectory: root,
    });
    if (!Number.isSafeInteger(handle.pid) || (handle.pid ?? 0) <= 0) {
        await handle.destroy();
        throw new Error("Native Vulkan launcher returned an invalid target PID");
    }
    const pid = handle.pid!;
    let available = true;
    let running = true;
    const markStopped = (): void => {
        running = false;
    };
    handle.events?.on?.("destroyed", markStopped);
    handle.events?.on?.("disconnect", markStopped);
    handle.on?.("disconnect", markStopped);
    const launchedAddon: NativeOverlayAddon = {
        attach(requestedPid) {
            if (!available || requestedPid !== pid) {
                throw new Error("Launched Vulkan overlay handle is no longer available");
            }
            available = false;
            return handle;
        },
    };
    const { vulkanLaunch: _launch, ...launchedOptions } = options;
    return new LinuxOverlayProvider(
        { ...launchedOptions, pids: [pid] },
        configStore,
        overlayRegistry,
        {
            ...dependencies,
            native: launchedAddon,
            foregroundTracker: dependencies.foregroundTracker ?? new LinuxPidTracker(pid),
            discoverPids: dependencies.discoverPids
                ?? (async () => new Set(running ? [pid] : [])),
        },
    );
}

/** Linux provider backed by the Rust N-API overlay host. */
export class LinuxOverlayProvider extends OverlayManager {
    constructor(
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        dependencies: LinuxOverlayProviderDependencies = {},
    ) {
        const native = dependencies.native;
        const loadNative = dependencies.loadNative ?? defaultNativeLoader;
        const initialPid = options.pids[0] ?? null;
        // Without an explicit `--pid` the manager must still find the game by
        // process name. Replacing discovery with `options.pids` left the
        // allowed set permanently empty, so the provider never attached on the
        // default Linux startup path.
        const candidates = new LinuxCandidateTracker();
        super(options, configStore, overlayRegistry, {
            ...dependencies,
            foregroundTracker: dependencies.foregroundTracker
                ?? (initialPid === null ? candidates : new LinuxPidTracker(initialPid)),
            discoverPids: dependencies.discoverPids
                ?? (async () => {
                    const discovered = await discoverTargetPids(options);
                    candidates.observe(discovered);
                    return discovered;
                }),
            initializeProcess: dependencies.initializeProcess ?? (async (
                pid: number,
                editingTarget: EditingTarget,
                visible: boolean,
            ): Promise<ManagedOverlayProcess> => {
                const addon = native ?? loadNative();
                const handle = await addon.attach(pid, nativeAttachOptions(options));
                if (handle.createSurface === undefined) {
                    return new NativeOverlayProcess(pid, handle);
                }
                return LinuxOverlayProcess.create(
                    pid,
                    handle,
                    editingTarget,
                    options,
                    configStore,
                    overlayRegistry,
                    visible,
                );
            }),
        });
    }
}
