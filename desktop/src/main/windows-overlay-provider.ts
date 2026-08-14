import type { ConfigStore } from "./config";
import {
    OverlayManager,
    type ManagedOverlayProcess,
    type OverlayManagerDependencies,
} from "./overlay-manager";
import { WindowsForegroundTracker } from "./foreground-tracker";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { EditingTarget } from "./types";
import type { RuntimeOptions } from "./options";

/** Windows provider retaining the asdf-backed attachment lifecycle. */
export class WindowsOverlayProvider extends OverlayManager {
    constructor(
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        dependencies: OverlayManagerDependencies = {},
    ) {
        super(options, configStore, overlayRegistry, {
            ...dependencies,
            foregroundTracker: dependencies.foregroundTracker ?? new WindowsForegroundTracker(),
            initializeProcess: dependencies.initializeProcess ?? (async (
                pid: number,
                editingTarget: EditingTarget,
                visible: boolean,
            ): Promise<ManagedOverlayProcess> => {
                const { OverlayProcess } = await import("./overlay-process");
                return OverlayProcess.initialize(
                    pid,
                    options,
                    configStore,
                    overlayRegistry,
                    editingTarget,
                    visible,
                );
            }),
        });
    }
}
