import type { ManagedOverlayProcess } from "./overlay-manager";
import type { NativeOverlayHandle } from "./linux-overlay-provider";

interface PaintingWebContents {
    isDestroyed?(): boolean;
    isPainting(): boolean;
    startPainting(): void;
    stopPainting(): void;
    invalidate(): void;
    on(event: "paint", listener: () => void): unknown;
    off(event: "paint", listener: () => void): unknown;
}

interface PaceableProcess extends ManagedOverlayProcess {
    readonly window: {
        webContents: PaintingWebContents;
    };
}

export interface ParsedFrameRequest {
    targetId: number;
    sequence: number;
}

export function parseFrameRequest(value: unknown): ParsedFrameRequest | null {
    if (typeof value !== "string") return null;
    const fields = value.split(":");
    if (fields.length !== 2) return null;
    const targetId = Number(fields[0]);
    const sequence = Number(fields[1]);
    if (!Number.isSafeInteger(targetId) || targetId <= 0
        || !Number.isSafeInteger(sequence) || sequence <= 0) return null;
    return { targetId, sequence };
}

function selectedTarget(value: unknown): number | null | undefined {
    if (typeof value !== "string") return undefined;
    const fields = value.split(":");
    if (fields[0] === "selected" && fields.length === 3) {
        const targetId = Number(fields[1]);
        return Number.isSafeInteger(targetId) && targetId >= 0 ? targetId || null : undefined;
    }
    if (fields[0] === "destroyed" && fields.length === 2) {
        const targetId = Number(fields[1]);
        return Number.isSafeInteger(targetId) && targetId > 0 ? -targetId : undefined;
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
    if (!(native.supportsFrameRequests?.() ?? false) || native.on === undefined) return;

    const webContents = process.window.webContents;
    let activeTarget: number | null = null;
    let visible = true;
    let destroyed = false;
    let pendingSequence = 0;
    let deliveredSequence = 0;

    const stop = (): void => {
        if (!webContents.isDestroyed?.() && webContents.isPainting()) {
            webContents.stopPainting();
        }
    };
    const requestPaint = (): void => {
        if (destroyed || !visible || activeTarget === null || webContents.isDestroyed?.()) return;
        if (!webContents.isPainting()) webContents.startPainting();
        webContents.invalidate();
    };
    const onTarget = (value: unknown): void => {
        const selected = selectedTarget(value);
        if (selected === undefined) return;
        if (selected !== null && selected < 0) {
            if (activeTarget === -selected) activeTarget = null;
        } else {
            activeTarget = selected;
        }
        stop();
    };
    const onFrameRequest = (value: unknown): void => {
        const request = parseFrameRequest(value);
        if (request === null) return;
        if (activeTarget !== null && request.targetId !== activeTarget) return;
        activeTarget ??= request.targetId;
        if (request.sequence <= pendingSequence) return;
        pendingSequence = request.sequence;
        requestPaint();
    };
    const onPaint = (): void => {
        deliveredSequence = pendingSequence;
        queueMicrotask(() => {
            if (destroyed) return;
            if (pendingSequence === deliveredSequence) stop();
            else requestPaint();
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
        if (!visible) stop();
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
