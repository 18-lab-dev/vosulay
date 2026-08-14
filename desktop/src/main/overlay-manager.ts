import { createRequire } from "node:module";
import type findProcessType from "find-process";
import EventEmitter from "node:events";

import type { RuntimeOptions } from "./options";
import type { ConfigStore } from "./config";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { EditingTarget } from "./types";
import type { OverlayProvider } from "./overlay-provider";
import { NoopForegroundTracker, type ForegroundTracker } from "./foreground-tracker";

const findProcessModule: unknown = createRequire(import.meta.url)("find-process");
if (!findProcessModule || typeof findProcessModule !== "object"
    || !("default" in findProcessModule) || typeof findProcessModule.default !== "function") {
    throw new Error("find-process did not expose its process lookup function");
}
const findProcess = findProcessModule.default as typeof findProcessType;

const PROCESS_SCAN_INTERVAL_MS = 1_000;

export interface ManagedOverlayProcess {
    readonly pid: number;
    readonly events: EventEmitter<{ destroyed: [] }>;
    readonly window?: {
        webContents?: {
            setFrameRate(frameRate: number): void;
        };
    };
    destroy(): Promise<void>;
    setEditing(editing: boolean): Promise<void>;
    setVisible(visible: boolean): Promise<void>;
    flushPendingSave(): Promise<void>;
    reload(): Promise<void>;
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


export class OverlayManager implements OverlayProvider {
    readonly events = new EventEmitter<{
        editingChanged: [editing: boolean];
        visibilityChanged: [visible: boolean];
    }>();
    private readonly foregroundTracker: ForegroundTracker;
    private readonly discoverPids: () => Promise<Set<number>>;
    private readonly initializeProcess: NonNullable<OverlayManagerDependencies["initializeProcess"]>;
    private readonly scheduleScan: NonNullable<OverlayManagerDependencies["scheduleScan"]>;
    private allowedPids = new Set<number>();
    private foregroundPid: number | null = null;
    private lastFocusedGamePid: number | null = null;
    private active: { pid: number; process: ManagedOverlayProcess } | null = null;
    private cancelScan: (() => void) | null = null;
    private scanPromise: Promise<void> | null = null;
    private stopPromise: Promise<void> | null = null;
    private transitionTail: Promise<void> = Promise.resolve();
    private stopped = true;
    private editingValue = false;
    private visibleValue = true;

    constructor(
        private readonly options: RuntimeOptions,
        private readonly configStore: ConfigStore,
        private readonly overlayRegistry: LocalOverlayRegistry,
        dependencies: OverlayManagerDependencies = {},
    ) {
        this.foregroundTracker = dependencies.foregroundTracker ?? new NoopForegroundTracker();
        this.discoverPids = dependencies.discoverPids ?? (() => this.findTargetPids());
        this.initializeProcess = dependencies.initializeProcess ?? (async () => {
            throw new Error("OverlayManager requires a platform overlay process initializer");
        });
        this.scheduleScan = dependencies.scheduleScan ?? ((scan) => {
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
        this.stopPromise = null;
        let receivedForegroundEvent = false;
        try {
            const initialForegroundPid = this.foregroundTracker.start((pid) => {
                if (this.stopped) {
                    return;
                }
                receivedForegroundEvent = true;
                this.foregroundPid = pid;
                if (pid !== null && this.allowedPids.has(pid)) {
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
        this.foregroundPid = null;
        this.lastFocusedGamePid = null;
        this.allowedPids.clear();
        this.cancelScan?.();
        this.cancelScan = null;
        try {
            this.foregroundTracker.stop();
        } catch (error) {
            console.error("Failed to stop foreground window tracking", error);
        }

        const pendingScan = this.scanPromise;
        if (pendingScan !== null) {
            await pendingScan;
        }
        await this.queueReconcile();
    }

    private scan(): void {
        if (this.stopped || this.scanPromise !== null) {
            return;
        }
        const pendingScan = this.runScan();
        this.scanPromise = pendingScan;
        void pendingScan.finally(() => {
            if (this.scanPromise === pendingScan) {
                this.scanPromise = null;
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

    private async findTargetPids(): Promise<Set<number>> {
        if (this.options.pids.length > 0) {
            return new Set(this.options.pids);
        }

        const matches = await Promise.all(
            this.options.processNames.map((name) => findProcess("name", name, true)),
        );
        return new Set(
            matches
                .flat()
                .map((process) => process.pid)
                .filter((pid) => Number.isSafeInteger(pid) && pid > 0),
        );
    }

    private queueReconcile(): Promise<void> {
        const transition = this.transitionTail.then(
            () => this.reconcileSelection(),
            () => this.reconcileSelection(),
        );
        this.transitionTail = transition.catch((error: unknown) => {
            console.error("Failed to reconcile foreground overlay", error);
        });
        return transition;
    }

    private desiredPid(): number | null {
        if (this.stopped) {
            return null;
        }
        if (this.foregroundPid !== null && this.allowedPids.has(this.foregroundPid)) {
            this.lastFocusedGamePid = this.foregroundPid;
        }
        if (this.lastFocusedGamePid === null || !this.allowedPids.has(this.lastFocusedGamePid)) {
            this.lastFocusedGamePid = null;
            return null;
        }
        return this.lastFocusedGamePid;
    }

    private async reconcileSelection(): Promise<void> {
        const desiredPid = this.desiredPid();
        if (this.active !== null && this.active.pid !== desiredPid) {
            const previous = this.active;
            this.active = null;
            await previous.process.destroy();
        }

        const currentDesiredPid = this.desiredPid();
        if (currentDesiredPid === null) {
            return;
        }
        if (this.active?.pid === currentDesiredPid) {
            await this.applyState(this.active);
            return;
        }

        let process: ManagedOverlayProcess;
        try {
            process = await this.initializeProcess(
                currentDesiredPid,
                this,
                this.visibleValue,
            );
        } catch (error) {
            console.error(`Failed to attach vosulay overlay to PID ${currentDesiredPid}`, error);
            return;
        }

        let destroyed = false;
        process.events.once("destroyed", () => {
            destroyed = true;
            if (this.active?.process === process) {
                this.active = null;
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
        } catch (error) {
            console.error(`Failed to apply overlay state for PID ${currentDesiredPid}`, error);
            await process.destroy();
            return;
        }

        if (destroyed || this.desiredPid() !== currentDesiredPid) {
            await process.destroy();
            return;
        }

        if (this.options.fpsLimit > 0) {
            process.window?.webContents?.setFrameRate(this.options.fpsLimit);
        }
        this.active = { pid: currentDesiredPid, process };
        console.log(`Attached vosulay overlay to PID ${currentDesiredPid}`);
    }

    private async applyState(
        active: { pid: number; process: ManagedOverlayProcess },
    ): Promise<void> {
        try {
            await active.process.setVisible(this.visibleValue);
            await active.process.setEditing(
                this.visibleValue && this.editingValue,
            );
        } catch (error) {
            console.error(`Failed to apply overlay state for PID ${active.pid}`, error);
            if (this.active?.process === active.process) {
                this.active = null;
            }
            await active.process.destroy();
        }
    }
}
