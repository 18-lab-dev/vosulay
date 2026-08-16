import { type JsonValue, isJsonString } from "./json";
import type { ManagedOverlayProcess } from "./overlay-manager";
import type { NativeOverlayHandle } from "./linux-overlay-provider";

interface PaintingWebContents {
    isDestroyed?: () => boolean;
    isPainting: () => boolean;
    startPainting: () => void;
    stopPainting: () => void;
    invalidate: () => void;
    on: (event: "paint", listener: () => void) => void;
    off: (event: "paint", listener: () => void) => void;
}

const FRAME_REQUEST_PART_COUNT = 2;
const SELECTED_TARGET_PART_COUNT = 3;
const DESTROYED_TARGET_PART_COUNT = 2;
const FRAME_REQUEST_MIN_ID = 0;
const FRAME_REQUEST_MIN_SEQUENCE = 0;
const MILLISECONDS_PER_SECOND = 1000;
const FRAME_INTERVAL_TOLERANCE_MS = 0.25;

interface PaceableProcess extends ManagedOverlayProcess {
    readonly window: {
        webContents: PaintingWebContents;
    };
}

export interface ParsedFrameRequest {
    targetId: number;
    sequence: number;
}

export function parseFrameRequest(value: JsonValue | undefined): ParsedFrameRequest | undefined {
    if (!isJsonString(value)) {
        return undefined;
    }
    const fields = value.split(":");
    if (fields.length !== FRAME_REQUEST_PART_COUNT) {
        return undefined;
    }
    const targetId = Number(fields[0]);
    const sequence = Number(fields[1]);
    if (
        !Number.isSafeInteger(targetId) ||
        targetId <= FRAME_REQUEST_MIN_ID ||
        !Number.isSafeInteger(sequence) ||
        sequence <= FRAME_REQUEST_MIN_SEQUENCE
    ) {
        return undefined;
    }
    return { sequence, targetId };
}

function selectedTarget(value: JsonValue | undefined): number | undefined {
    if (!isJsonString(value)) {
        return undefined;
    }
    const fields = value.split(":");
    if (fields[0] === "selected" && fields.length === SELECTED_TARGET_PART_COUNT) {
        const targetId = Number(fields[1]);
        return Number.isSafeInteger(targetId) && targetId >= FRAME_REQUEST_MIN_ID
            ? targetId || undefined
            : undefined;
    }
    if (fields[0] === "destroyed" && fields.length === DESTROYED_TARGET_PART_COUNT) {
        const targetId = Number(fields[1]);
        return Number.isSafeInteger(targetId) && targetId > FRAME_REQUEST_MIN_ID
            ? -targetId
            : undefined;
    }
    return undefined;
}

export function frameRequestDelayMs(lastRequestAtMs: number, nowMs: number, fpsLimit: number): number {
    const intervalMs = MILLISECONDS_PER_SECOND / fpsLimit;
    return Math.max(0, lastRequestAtMs + intervalMs - nowMs);
}

/**
 * Convert Electron OSR from fixed-rate painting to coalesced game-present
 * demand when the native backend negotiated CAP_FRAME_REQUEST. The native
 * present hook only advances an atomic demand sequence; socket I/O remains on
 * its control thread. This listener applies the configured FPS ceiling before
 * Chromium is invalidated, so a 1000 Hz game cannot make a 240 FPS overlay
 * render 1000 times per second.
 */
export function enableNativeFramePacing(
    process: PaceableProcess,
    native: NativeOverlayHandle,
    fpsLimit: number,
): void {
    if (
        !(native.supportsFrameRequests?.() ?? false) ||
        native.on === undefined ||
        !Number.isFinite(fpsLimit) ||
        fpsLimit <= 0
    ) {
        return;
    }

    const { webContents } = process.window;
    let activeTarget: number | undefined = undefined;
    let deliveredSequence = 0;
    let destroyed = false;
    let lastPaintRequestAtMs = Number.NEGATIVE_INFINITY;
    let paceTimer: ReturnType<typeof setTimeout> | undefined = undefined;
    let paintInFlight = false;
    let pendingSequence = 0;
    let visible = true;

    const stop = (): void => {
        if (!webContents.isDestroyed?.() && webContents.isPainting()) {
            webContents.stopPainting();
        }
    };
    const cancelPaceTimer = (): void => {
        if (paceTimer === undefined) {
            return;
        }
        clearTimeout(paceTimer);
        paceTimer = undefined;
    };
    const resetDemand = (): void => {
        cancelPaceTimer();
        paintInFlight = false;
        pendingSequence = 0;
        deliveredSequence = 0;
        lastPaintRequestAtMs = Number.NEGATIVE_INFINITY;
    };
    const requestPaint = (): void => {
        if (
            destroyed ||
            !visible ||
            activeTarget === undefined ||
            webContents.isDestroyed?.() ||
            paintInFlight ||
            paceTimer !== undefined ||
            pendingSequence <= deliveredSequence
        ) {
            return;
        }
        const nowMs = performance.now();
        const delayMs = frameRequestDelayMs(lastPaintRequestAtMs, nowMs, fpsLimit);
        if (delayMs > FRAME_INTERVAL_TOLERANCE_MS) {
            paceTimer = setTimeout(() => {
                paceTimer = undefined;
                requestPaint();
            }, delayMs);
            return;
        }
        paintInFlight = true;
        lastPaintRequestAtMs = nowMs;
        if (!webContents.isPainting()) {
            webContents.startPainting();
        }
        webContents.invalidate();
    };
    const onTarget = (payload: JsonValue | undefined): void => {
        const selected = selectedTarget(payload);
        if (selected === undefined) {
            return;
        }
        if (selected < 0) {
            if (activeTarget === -selected) {
                activeTarget = undefined;
                resetDemand();
            }
        } else if (activeTarget !== selected) {
            activeTarget = selected;
            resetDemand();
        }
        stop();
    };
    const onFrameRequest = (payload: JsonValue | undefined): void => {
        const request = parseFrameRequest(payload);
        if (request === undefined) {
            return;
        }
        if (activeTarget !== undefined && request.targetId !== activeTarget) {
            return;
        }
        activeTarget ??= request.targetId;
        if (request.sequence <= pendingSequence) {
            return;
        }
        pendingSequence = request.sequence;
        requestPaint();
    };
    const onPaint = (): void => {
        if (!paintInFlight) {
            return;
        }
        paintInFlight = false;
        deliveredSequence = pendingSequence;
        queueMicrotask(() => {
            if (destroyed) {
                return;
            }
            stop();
            requestPaint();
        });
    };

    native.on("target", onTarget);
    native.on("frame-request", onFrameRequest);
    webContents.on("paint", onPaint);
    stop();

    const originalVisible = process.setVisible.bind(process);
    process.setVisible = async (nextVisible: boolean): Promise<void> => {
        await originalVisible(nextVisible);
        visible = nextVisible;
        if (!visible) {
            resetDemand();
        }
        stop();
    };

    const originalDestroy = process.destroy.bind(process);
    process.destroy = async (): Promise<void> => {
        if (!destroyed) {
            destroyed = true;
            cancelPaceTimer();
            native.off?.("target", onTarget);
            native.off?.("frame-request", onFrameRequest);
            webContents.off("paint", onPaint);
            stop();
        }
        await originalDestroy();
    };
}
