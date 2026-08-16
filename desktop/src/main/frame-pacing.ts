import { type JsonValue, isJsonString } from "./json";
import type { ManagedOverlayProcess } from "./overlay-manager";
import type { NativeOverlayHandle } from "./linux-overlay-provider";
interface PaintingWebContents {
    isDestroyed?: () => boolean;
    isPainting: () => boolean;
    startPainting: () => void;
    stopPainting: () => void;
    invalidate: () => void;
    setFrameRate: (frameRate: number) => void;
    on: (event: "paint", listener: () => void) => void;
    off: (event: "paint", listener: () => void) => void;
}

const FRAME_REQUEST_PART_COUNT = 2;
const SELECTED_TARGET_PART_COUNT = 3;
const DESTROYED_TARGET_PART_COUNT = 2;
const FRAME_REQUEST_MIN_ID = 0;
const FRAME_REQUEST_MIN_SEQUENCE = 0;

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

/**
 * Convert Electron OSR from fixed-rate painting to coalesced game-present
 * demand when the native backend negotiated CAP_FRAME_REQUEST. No socket I/O
 * or JS callback runs on the game presentation thread: the native control
 * actor forwards only the newest demand sequence to this listener.
 */
export function enableNativeFramePacing(
    process: PaceableProcess,
    native: NativeOverlayHandle,
): void {
    if (!(native.supportsFrameRequests?.() ?? false) || native.on === undefined) {
        return;
    }

    const { webContents } = process.window;
    let activeTarget: number | undefined = undefined;
    let deliveredSequence = 0;
    let destroyed = false;
    let pendingSequence = 0;
    let visible = true;

    const stop = (): void => {
        if (!webContents.isDestroyed?.() && webContents.isPainting()) {
            webContents.stopPainting();
        }
    };
    const requestPaint = (): void => {
        if (destroyed || !visible || activeTarget === undefined || webContents.isDestroyed?.()) {
            return;
        }
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
            }
        } else {
            activeTarget = selected;
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
        deliveredSequence = pendingSequence;
        queueMicrotask(() => {
            if (destroyed) {
                return;
            }
            if (pendingSequence === deliveredSequence) {
                stop();
            } else {
                requestPaint();
            }
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
            stop();
        }
    };

    const originalDestroy = process.destroy.bind(process);
    process.destroy = async (): Promise<void> => {
        if (!destroyed) {
            destroyed = true;
            native.off?.("target", onTarget);
            native.off?.("frame-request", onFrameRequest);
            webContents.off("paint", onPaint);
            stop();
        }
        await originalDestroy();
    };
}
