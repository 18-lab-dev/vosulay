import { createRequire } from "node:module";

export interface ActiveWindowInfo {
    pid: number;
}

export interface ActiveWindowApi {
    initialize(): void;
    getActiveWindow(): ActiveWindowInfo;
    subscribe(callback: (windowInfo: ActiveWindowInfo | null) => void): number;
    unsubscribe(watchId: number): void;
}

interface ActiveWindowModule {
    ActiveWindow: ActiveWindowApi;
}

export interface ForegroundTracker {
    start(onChanged: (pid: number | null) => void): number | null;
    stop(): void;
}

export interface ForegroundTrackerDependencies {
    loadActiveWindow?: () => ActiveWindowApi;
    scheduleReadback?: (poll: () => void) => () => void;
}

function validPid(windowInfo: ActiveWindowInfo | null): number | null {
    const pid = windowInfo?.pid;
    return Number.isSafeInteger(pid) && pid !== undefined && pid > 0 ? pid : null;
}

export class WindowsForegroundTracker implements ForegroundTracker {
    private readonly loadActiveWindow: () => ActiveWindowApi;
    private readonly scheduleReadback: (poll: () => void) => () => void;
    private activeWindow: ActiveWindowApi | null = null;
    private watchId: number | null = null;
    private cancelReadback: (() => void) | null = null;
    private lastPid: number | null = null;

    constructor(dependencies: ForegroundTrackerDependencies = {}) {
        this.loadActiveWindow = dependencies.loadActiveWindow ?? (() => {
            const module = createRequire(import.meta.url)("@paymoapp/active-window") as ActiveWindowModule;
            return module.ActiveWindow;
        });
        this.scheduleReadback = dependencies.scheduleReadback ?? ((poll) => {
            const timer = setInterval(poll, 100);
            return () => clearInterval(timer);
        });
    }

    start(onChanged: (pid: number | null) => void): number | null {
        if (this.watchId !== null || this.cancelReadback !== null) {
            throw new Error("Foreground tracker is already running");
        }

        this.activeWindow = this.loadActiveWindow();
        this.activeWindow.initialize();

        let pendingPid: number | null | undefined;
        let seeding = true;
        const publish = (pid: number | null): void => {
            if (pid !== this.lastPid) {
                this.lastPid = pid;
                onChanged(pid);
            }
        };
        this.watchId = this.activeWindow.subscribe((windowInfo) => {
            const pid = validPid(windowInfo);
            if (seeding) {
                pendingPid = pid;
            } else {
                publish(pid);
            }
        });

        let currentPid: number | null;
        try {
            currentPid = validPid(this.activeWindow.getActiveWindow());
        } catch {
            currentPid = pendingPid ?? null;
        }
        this.lastPid = currentPid;
        seeding = false;

        this.cancelReadback = this.scheduleReadback(() => {
            const activeWindow = this.activeWindow;
            if (activeWindow === null) {
                return;
            }
            try {
                publish(validPid(activeWindow.getActiveWindow()));
            } catch {
                publish(null);
            }
        });
        return currentPid;
    }

    stop(): void {
        const activeWindow = this.activeWindow;
        const watchId = this.watchId;
        this.activeWindow = null;
        this.watchId = null;
        this.lastPid = null;
        this.cancelReadback?.();
        this.cancelReadback = null;
        if (activeWindow !== null && watchId !== null) {
            activeWindow.unsubscribe(watchId);
        }
    }
}
