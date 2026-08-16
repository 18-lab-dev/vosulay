import { type ForegroundTracker, NoopForegroundTracker } from "./foreground-tracker";
import type { ConfigStore } from "./config";
import type { EditingTarget } from "./types";
import EventEmitter from "node:events";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { OverlayProvider } from "./overlay-provider";
import type { RuntimeOptions } from "./options";
import { createRequire } from "node:module";
import type findProcessType from "find-process";

const findProcessModule: { default?: unknown } = createRequire(import.meta.url)("find-process");
function isFindProcessModule(value: { default?: unknown } | undefined): value is FindProcessModule {
    return typeof value === "object" && value !== null && typeof value.default === "function";
}

interface FindProcessModule {
    default: typeof findProcessType;
}

if (!isFindProcessModule(findProcessModule)) {
    throw new Error("find-process did not expose its process lookup function");
}

const findProcess = findProcessModule.default;
const PROCESS_SCAN_INTERVAL_MS = 1000;
const FRAME_RATE_DISABLED = 0;

export interface ManagedOverlayProcess {
    readonly pid: number;
    readonly events: EventEmitter<{ destroyed: [] }>;
    readonly window?: {
        webContents?: {
            setFrameRate: (frameRate: number) => void;
        };
    };
    destroy: () => Promise<void>;
    setEditing: (editing: boolean) => Promise<void>;
    setVisible: (visible: boolean) => Promise<void>;
    flushPendingSave: () => Promise<void>;
    reload: () => Promise<void>;
}

export interface OverlayManagerDependencies {
    foregroundTracker?: ForegroundTracker;
    discoverPids?: () => Promise<Set<number>>;
    initializeProcess?: (
        pid: number,
        editingTarget: EditingTarget,
        visible: boolean,
    ) => Promise<ManagedOverlayProcess>;
    scheduleScan?: (scan: () => void) => () => void;
}

/// Explicit `--pid` values win; otherwise the configured process names are
/// resolved. Platform providers must reuse this rather than substituting their
/// own set, or a default startup with no `--pid` discovers nothing at all.
export async function discoverTargetPids(options: RuntimeOptions): Promise<Set<number>> {
    if (options.pids.length > 0) {
        return new Set(options.pids);
    }
    const matches = await Promise.all(
        options.processNames.map((name) => findProcess("name", name, true)),
    );
    return new Set(
        matches
            .flat()
            .map((process) => process.pid)
            .filter((pid) => Number.isSafeInteger(pid) && pid > 0),
    );
}

export class OverlayManager implements OverlayProvider {
    readonly events = new EventEmitter<{
        editingChanged: [editing: boolean];
        visibilityChanged: [visible: boolean];
    }>();
    private readonly options: RuntimeOptions;
    private readonly configStore: ConfigStore;
    private readonly overlayRegistry: LocalOverlayRegistry;
    private readonly foregroundTracker: ForegroundTracker;
    private readonly discoverPids: () => Promise<Set<number>>;
    private readonly initializeProcess: NonNullable<
        OverlayManagerDependencies["initializeProcess"]
    >;
    private readonly scheduleScan: NonNullable<OverlayManagerDependencies["scheduleScan"]>;
    private allowedPids = new Set<number>();
    private foregroundPid: number | undefined = undefined;
    private lastFocusedGamePid: number | undefined = undefined;
    private active: { pid: number; process: ManagedOverlayProcess } | undefined = undefined;
    private cancelScan: (() => void) | undefined = undefined;
    private scanPromise: Promise<void> | undefined = undefined;
    private stopPromise: Promise<void> | undefined = undefined;
    private transitionTail: Promise<void> = Promise.resolve();
    private stopped = true;
    private editingValue = false;
    private visibleValue = true;

    constructor(
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        dependencies: OverlayManagerDependencies = {},
    ) {
        this.options = options;
        this.configStore = configStore;
        this.overlayRegistry = overlayRegistry;
        this.foregroundTracker = dependencies.foregroundTracker ?? new NoopForegroundTracker();
        this.discoverPids = dependencies.discoverPids ?? (() => this.findTargetPids());
        this.initializeProcess =
            dependencies.initializeProcess ??
            (() =>
                Promise.reject(
                    new Error("OverlayManager requires a platform overlay process initializer"),
                ));
        this.scheduleScan =
            dependencies.scheduleScan ??
            ((scan) => {
                const timer = setInterval(scan, PROCESS_SCAN_INTERVAL_MS);
                return () => clearInterval(timer);
            });
    }

    get editing(): boolean {
        return this.editingValue;
    }

    get visible(): boolean {
        return this.visibleValue;
    }

    start(): void {
        if (!this.stopped) {
            return;
        }
        this.stopped = false;
        this.stopPromise = undefined;
        let receivedForegroundEvent = false;
        try {
            const initialForegroundPid = this.foregroundTracker.start((pid) => {
                if (this.stopped) {
                    return;
                }
                receivedForegroundEvent = true;
                this.foregroundPid = pid;
                if (pid !== undefined && this.allowedPids.has(pid)) {
                    this.lastFocusedGamePid = pid;
                }
                void this.queueReconcile();
            });
            if (!receivedForegroundEvent) {
                this.foregroundPid = initialForegroundPid;
            }
        } catch (error) {
            this.stopped = true;
            throw error;
        }
        this.scan();
        this.cancelScan = this.scheduleScan(() => this.scan());
    }

    async setEditing(editing: boolean): Promise<void> {
        if (this.stopped) {
            return;
        }
        this.editingValue = editing;
        await this.queueReconcile();
        this.events.emit("editingChanged", this.editingValue);
    }

    async toggleEditing(): Promise<void> {
        await this.setEditing(!this.editingValue);
    }

    async setVisibility(visible: boolean): Promise<void> {
        if (visible === this.visibleValue) {
            return;
        }
        this.visibleValue = visible;
        if (!this.stopped) {
            await this.queueReconcile();
        }
        this.events.emit("visibilityChanged", this.visibleValue);
    }

    async toggleVisibility(): Promise<void> {
        await this.setVisibility(!this.visibleValue);
    }

    async flushAll(): Promise<void> {
        await this.active?.process.flushPendingSave();
    }

    async reloadAll(): Promise<void> {
        const process = this.active?.process;
        if (process === undefined) {
            return;
        }
        try {
            await process.reload();
        } catch (error) {
            console.error("Failed to reload overlay", error);
        }
    }

    stop(): Promise<void> {
        this.stopPromise ??= this.finishStop();
        return this.stopPromise;
    }

    private async finishStop(): Promise<void> {
        this.stopped = true;
        this.editingValue = false;
        this.visibleValue = true;
        this.foregroundPid = undefined;
        this.lastFocusedGamePid = undefined;
        this.allowedPids.clear();
        this.cancelScan?.();
        this.cancelScan = undefined;
        try {
            this.foregroundTracker.stop();
        } catch (error) {
            console.error("Failed to stop foreground window tracking", error);
        }

        const pendingScan = this.scanPromise;
        if (pendingScan !== undefined) {
            await pendingScan;
        }
        await this.queueReconcile();
    }

    private scan(): void {
        if (this.stopped || this.scanPromise !== undefined) {
            return;
        }
        const pendingScan = this.runScan();
        this.scanPromise = pendingScan;
        void pendingScan.finally(() => {
            if (this.scanPromise === pendingScan) {
                this.scanPromise = undefined;
            }
        });
    }

    private async runScan(): Promise<void> {
        try {
            const targetPids = await this.discoverPids();
            if (this.stopped) {
                return;
            }
            this.allowedPids = targetPids;
            await this.queueReconcile();
        } catch (error) {
            console.error("Failed to discover osu! processes", error);
        }
    }

    private findTargetPids(): Promise<Set<number>> {
        return discoverTargetPids(this.options);
    }

    private queueReconcile(): Promise<void> {
        const transition = this.transitionTail.then(
            () => this.reconcileSelection(),
            () => this.reconcileSelection(),
        );
        this.transitionTail = transition.catch((cause: unknown) => {
            console.error("Failed to reconcile foreground overlay", cause);
        });
        return transition;
    }

    private desiredPid(): number | undefined {
        if (this.stopped) {
            return undefined;
        }
        if (this.foregroundPid !== undefined && this.allowedPids.has(this.foregroundPid)) {
            this.lastFocusedGamePid = this.foregroundPid;
        }
        if (
            this.lastFocusedGamePid === undefined ||
            !this.allowedPids.has(this.lastFocusedGamePid)
        ) {
            this.lastFocusedGamePid = undefined;
            return undefined;
        }
        return this.lastFocusedGamePid;
    }

    private async reconcileSelection(): Promise<void> {
        const desiredPid = this.desiredPid();
        if (this.active !== undefined && this.active.pid !== desiredPid) {
            const previous = this.active;
            this.active = undefined;
            await previous.process.destroy();
        }

        const currentDesiredPid = this.desiredPid();
        if (currentDesiredPid === undefined) {
            return;
        }
        if (this.active?.pid === currentDesiredPid) {
            await this.applyState(this.active);
            return;
        }

        let process: ManagedOverlayProcess | undefined = undefined;
        try {
            process = await this.initializeProcess(currentDesiredPid, this, this.visibleValue);
        } catch (cause) {
            console.error(`Failed to attach vosulay overlay to PID ${currentDesiredPid}`, cause);
            return;
        }
        if (process === undefined) {
            return;
        }

        let destroyed = false;
        process.events.once("destroyed", () => {
            destroyed = true;
            if (this.active?.process === process) {
                this.active = undefined;
                if (!this.stopped) {
                    void this.queueReconcile();
                }
            }
        });

        if (destroyed || this.desiredPid() !== currentDesiredPid) {
            await process.destroy();
            return;
        }

        try {
            await process.setVisible(this.visibleValue);
            await process.setEditing(this.visibleValue && this.editingValue);
        } catch (cause) {
            console.error(`Failed to apply overlay state for PID ${currentDesiredPid}`, cause);
            await process.destroy();
            return;
        }
        if (destroyed || this.desiredPid() !== currentDesiredPid) {
            await process.destroy();
            return;
        }

        if (this.options.fpsLimit > FRAME_RATE_DISABLED) {
            process.window?.webContents?.setFrameRate(this.options.fpsLimit);
        }
        this.active = { pid: currentDesiredPid, process };
        console.log(`Attached vosulay overlay to PID ${currentDesiredPid}`);
    }

    private async applyState(active: {
        pid: number;
        process: ManagedOverlayProcess;
    }): Promise<void> {
        try {
            await active.process.setVisible(this.visibleValue);
            await active.process.setEditing(this.visibleValue && this.editingValue);
        } catch (cause) {
            console.error(`Failed to apply overlay state for PID ${active.pid}`, cause);
            if (this.active?.process === active.process) {
                this.active = undefined;
            }
            await active.process.destroy();
        }
    }
}
