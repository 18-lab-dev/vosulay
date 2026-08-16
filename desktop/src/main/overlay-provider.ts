import type { EditingTarget } from "./types";
import type EventEmitter from "node:events";

/**
 * Host-facing overlay lifecycle. Providers own platform-specific attachment,
 * rendering, and input details while the Electron main process consumes this
 * contract for commands and shutdown.
 */
export interface OverlayProvider extends EditingTarget {
    readonly events: EventEmitter<{
        editingChanged: [editing: boolean];
        visibilityChanged: [visible: boolean];
    }>;
    readonly visible: boolean;
    setVisibility: (visible: boolean) => Promise<void>;
    toggleVisibility: () => Promise<void>;
    flushAll: () => Promise<void>;
    reloadAll: () => Promise<void>;
    start: () => void;
    stop: () => Promise<void>;
}
