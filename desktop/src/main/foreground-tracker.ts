import { createRequire } from "node:module";

const READBACK_INTERVAL_MS = 100;

export interface ActiveWindowInfo {
    pid: number;
}

export interface ActiveWindowApi {
    initialize: () => void;
    getActiveWindow: () => ActiveWindowInfo;
    subscribe: (callback: (windowInfo: ActiveWindowInfo | undefined) => void) => number;
    unsubscribe: (watchId: number) => void;
}

interface ActiveWindowModule {
    ActiveWindow: ActiveWindowApi;
}

export interface ForegroundTracker {
    start: (onChanged: (pid: number | undefined) => void) => number | undefined;
    stop: () => void;
}

export class NoopForegroundTracker implements ForegroundTracker {
    start(_onChanged: (pid: number | undefined) => void): number | undefined {
        return undefined;
    }

    stop(): void {}
}

export interface ForegroundTrackerDependencies {
    loadActiveWindow?: () => ActiveWindowApi;
    scheduleReadback?: (poll: () => void) => () => void;
}

function validPid(windowInfo: ActiveWindowInfo | undefined): number | undefined {
    const pid = windowInfo?.pid;
    return Number.isSafeInteger(pid) && pid !== undefined && pid > 0 ? pid : undefined;
}

export class WindowsForegroundTracker implements ForegroundTracker {
    private readonly loadActiveWindow: () => ActiveWindowApi;
    private readonly scheduleReadback: (poll: () => void) => () => void;
    private activeWindow: ActiveWindowApi | undefined = undefined;
    private watchId: number | undefined = undefined;
    private cancelReadback: (() => void) | undefined = undefined;
    private lastPid: number | undefined = undefined;
    constructor(dependencies: ForegroundTrackerDependencies = {}) {
        this.loadActiveWindow =
            dependencies.loadActiveWindow ??
            (() => {
                /* SAFETY: The active-window package exports a module object with
                 * an `ActiveWindow` property containing the tracker API this
                 * code consumes.
                 */
                const module = createRequire(import.meta.url)(
                    "@paymoapp/active-window",
                ) as ActiveWindowModule;
                return module.ActiveWindow;
            });
        this.scheduleReadback =
            dependencies.scheduleReadback ??
            ((poll) => {
                const timer = setInterval(poll, READBACK_INTERVAL_MS);
                return () => clearInterval(timer);
            });
    }
    start(onChanged: (pid: number | undefined) => void): number | undefined {
        if (this.watchId !== undefined || this.cancelReadback !== undefined) {
            throw new Error("Foreground tracker is already running");
        }

        this.activeWindow = this.loadActiveWindow();
        this.activeWindow.initialize();

        let pendingPid: number | undefined = undefined;
        let seeding = true;
        const publish = (pid?: number): void => {
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

        let currentPid: number | undefined = undefined;
        try {
            currentPid = validPid(this.activeWindow.getActiveWindow());
        } catch {
            currentPid = pendingPid ?? undefined;
        }
        this.lastPid = currentPid;
        seeding = false;

        this.cancelReadback = this.scheduleReadback(() => {
            const { activeWindow } = this;
            if (activeWindow === undefined) {
                return;
            }
            try {
                publish(validPid(activeWindow.getActiveWindow()));
            } catch {
                publish();
            }
        });
        return currentPid;
    }

    stop(): void {
        const { activeWindow } = this;
        const { watchId } = this;
        this.activeWindow = undefined;
        this.watchId = undefined;
        this.lastPid = undefined;
        this.cancelReadback?.();
        this.cancelReadback = undefined;
        if (activeWindow !== undefined && watchId !== undefined) {
            activeWindow.unsubscribe(watchId);
        }
    }
}
