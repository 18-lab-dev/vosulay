import { contextBridge, ipcRenderer } from "electron";

const FRAME_RATE_CHANNEL = "vosulay:frame-rate";
const MILLISECONDS_PER_SECOND = 1000;
const FRAME_INTERVAL_TOLERANCE_MS = 0.5;

function isFrameRate(value: unknown): value is number {
    return typeof value === "number" && Number.isFinite(value) && Number.isInteger(value) && value > 0;
}

function installAnimationFrameLimiter(
    fpsLimit: number,
    millisecondsPerSecond: number,
    frameIntervalToleranceMs: number,
): void {
    const marker = "__vosulayAnimationFrameLimit";
    if (Object.hasOwn(window, marker)) {
        return;
    }

    const minFrameIntervalMs = millisecondsPerSecond / fpsLimit;
    const nativeRequestAnimationFrame = window.requestAnimationFrame.bind(window);
    const nativeCancelAnimationFrame = window.cancelAnimationFrame.bind(window);
    const pendingAnimationFrames = new Map<number, FrameRequestCallback>();
    let lastTimestamp: number | undefined = undefined;
    let nextAnimationFrameId = 0;
    let scheduledAnimationFrame: number | undefined = undefined;
    let scheduledTimeout: number | undefined = undefined;

    function clearScheduledTimeout(): void {
        if (scheduledTimeout === undefined) {
            return;
        }
        window.clearTimeout(scheduledTimeout);
        scheduledTimeout = undefined;
    }

    function clearScheduledAnimationFrame(): void {
        if (scheduledAnimationFrame === undefined) {
            return;
        }
        nativeCancelAnimationFrame(scheduledAnimationFrame);
        scheduledAnimationFrame = undefined;
    }

    function scheduleAnimationFramePump(): void {
        if (
            pendingAnimationFrames.size === 0 ||
            scheduledAnimationFrame !== undefined ||
            scheduledTimeout !== undefined
        ) {
            return;
        }
        if (lastTimestamp !== undefined) {
            const remainingMs = lastTimestamp + minFrameIntervalMs - performance.now();
            const delayMs = remainingMs - 1;
            if (delayMs > frameIntervalToleranceMs) {
                scheduledTimeout = window.setTimeout(() => {
                    scheduledTimeout = undefined;
                    scheduleAnimationFramePump();
                }, delayMs);
                return;
            }
        }
        scheduledAnimationFrame = nativeRequestAnimationFrame(flushNativeAnimationFrames);
    }

    function flushNativeAnimationFrames(timestamp: number): void {
        scheduledAnimationFrame = undefined;
        if (
            lastTimestamp !== undefined &&
            timestamp + frameIntervalToleranceMs - lastTimestamp < minFrameIntervalMs
        ) {
            scheduleAnimationFramePump();
            return;
        }
        lastTimestamp = timestamp;
        const callbacks = [...pendingAnimationFrames.values()];
        pendingAnimationFrames.clear();
        for (const callback of callbacks) {
            try {
                callback(timestamp);
            } catch (error) {
                window.setTimeout(() => {
                    throw error;
                }, 0);
            }
        }
        scheduleAnimationFramePump();
    }

    window.requestAnimationFrame = (callback: FrameRequestCallback): number => {
        nextAnimationFrameId += 1;
        pendingAnimationFrames.set(nextAnimationFrameId, callback);
        scheduleAnimationFramePump();
        return nextAnimationFrameId;
    };
    window.cancelAnimationFrame = (animationFrameId: number): void => {
        pendingAnimationFrames.delete(animationFrameId);
        if (pendingAnimationFrames.size === 0) {
            clearScheduledTimeout();
            clearScheduledAnimationFrame();
        }
    };
    Object.defineProperty(window, marker, {
        configurable: false,
        enumerable: false,
        value: fpsLimit,
        writable: false,
    });
}

const configuredFrameRate: unknown = ipcRenderer.sendSync(FRAME_RATE_CHANNEL);
if (isFrameRate(configuredFrameRate)) {
    contextBridge.executeInMainWorld({
        args: [configuredFrameRate, MILLISECONDS_PER_SECOND, FRAME_INTERVAL_TOLERANCE_MS],
        func: installAnimationFrameLimiter,
    });
}
