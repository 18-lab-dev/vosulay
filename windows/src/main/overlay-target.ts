import type { GpuLuid } from "@asdf-overlay/core";

export interface OverlayTarget {
    hwnd: number;
    width: number;
    height: number;
    luid: GpuLuid;
}

function isViableTarget(target: OverlayTarget): boolean {
    return Number.isSafeInteger(target.hwnd)
        && target.hwnd > 0
        && Number.isFinite(target.width)
        && Number.isFinite(target.height)
        && target.width > 1
        && target.height > 1;
}

export function chooseOverlayTarget(targets: Iterable<OverlayTarget>): OverlayTarget | null {
    let best: OverlayTarget | null = null;
    for (const target of targets) {
        if (!isViableTarget(target)) {
            continue;
        }
        if (best === null) {
            best = target;
            continue;
        }

        const area = target.width * target.height;
        const bestArea = best.width * best.height;
        if (
            area > bestArea
            || (area === bestArea && target.width > best.width)
            || (
                area === bestArea
                && target.width === best.width
                && target.height > best.height
            )
            || (
                area === bestArea
                && target.width === best.width
                && target.height === best.height
                && target.hwnd < best.hwnd
            )
        ) {
            best = target;
        }
    }
    return best;
}
