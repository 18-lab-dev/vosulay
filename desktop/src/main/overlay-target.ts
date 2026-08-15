export interface GpuLuid {
    high: number;
    low: number;
}

export interface OverlayTarget {
    hwnd: number;
    width: number;
    height: number;
    luid: GpuLuid;
}

function isViableTarget(target: OverlayTarget): boolean {
    return (
        Number.isSafeInteger(target.hwnd) &&
        target.hwnd > 0 &&
        Number.isFinite(target.width) &&
        Number.isFinite(target.height) &&
        target.width > 1 &&
        target.height > 1
    );
}

export function chooseOverlayTarget(targets: Iterable<OverlayTarget>): OverlayTarget | undefined {
    let best: OverlayTarget | undefined = undefined;
    for (const target of targets) {
        if (!isViableTarget(target)) {
            continue;
        }
        if (best === undefined) {
            best = target;
            continue;
        }

        const area = target.width * target.height,
            bestArea = best.width * best.height;
        if (
            area > bestArea ||
            (area === bestArea && target.width > best.width) ||
            (area === bestArea && target.width === best.width && target.height > best.height) ||
            (area === bestArea &&
                target.width === best.width &&
                target.height === best.height &&
                target.hwnd < best.hwnd)
        ) {
            best = target;
        }
    }
    return best;
}
