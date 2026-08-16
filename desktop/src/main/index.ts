import { ConfigStore, loadConfigSync, sharedArrayBufferBlinkSwitch } from "./config";
import {
    LocalOverlayRegistry,
    installCustomProtocols,
    loadLocalOverlayCapabilityKey,
    registerCustomSchemes,
} from "./local-overlays";
import {
    Menu,
    type MenuItemConstructorOptions,
    Tray,
    app,
    globalShortcut,
    nativeImage,
    session,
} from "electron";
import { type ParsedInvocation, type RuntimeOptions, parseInvocation } from "./options";
import { type OverlayProvider } from "./overlay-provider";
import { RemoteOverlayServer } from "./remote-server";
import { createOverlayProvider } from "./provider-factory";
import { mkdirSync } from "node:fs";
import { resolveStoragePaths } from "./storage-path";

const APP_EXIT_FAILURE = 1;
const APP_EXIT_SUCCESS = 0;
const TRAY_ALPHA_CHANNEL = 3;
const TRAY_BLUE_CHANNEL = 2;
const TRAY_BUFFER_SIZE_BYTES = 4;
const TRAY_GREEN_CHANNEL = 1;
const TRAY_ICON_SIZE = 16;
const TRAY_LOOP_LIMIT = TRAY_ICON_SIZE - 1;
const TRAY_LOOP_PADDING = 1;
const TRAY_OPAQUE_ALPHA = 255;
const TRAY_OUTER_RED = 233;
const TRAY_RED_CHANNEL = 0;
const TRAY_RING_GREEN = 116;
const TRAY_RING_BLUE = 165;
const TRAY_RING_WIDTH = 4;

const EDIT_ACCELERATOR = "Control+Shift+Space";
const VISIBILITY_ACCELERATOR = "Control+Alt+Shift+H";

if (process.platform === "win32") {
    app.commandLine.appendSwitch("force_high_performance_gpu");
    app.commandLine.appendSwitch("disable-direct-composition");
} else if (process.platform === "linux") {
    // Electron's Linux shared-texture OSR needs the ANGLE Vulkan path on
    // NVIDIA; the default Ganesh/GL representation cannot import Ozone's
    // Native pixmap backing and emits no paint texture.
    app.commandLine.appendSwitch("use-angle", "vulkan");
    app.commandLine.appendSwitch("enable-features", "Vulkan");
    // Chromium still gates WebGPU behind an unsafe opt-in on some Linux GPU
    // Configurations. Overlay iframes already delegate the `webgpu` feature;
    // This enables the browser-process side used by GPU-backed counters.
    app.commandLine.appendSwitch("enable-unsafe-webgpu");
}
app.commandLine.appendSwitch("high-dpi-support", "1");
app.commandLine.appendSwitch("force-device-scale-factor", "1");

const storagePaths = resolveStoragePaths(
    app.getPath("appData"),
    process.env.PORTABLE_EXECUTABLE_DIR,
);
mkdirSync(storagePaths.dataDirectory, { recursive: true });
const localOverlayCapabilityKey = loadLocalOverlayCapabilityKey(storagePaths.dataDirectory);
app.setPath("userData", storagePaths.dataDirectory);
const startupConfig = loadConfigSync(storagePaths.configPath);
app.commandLine.appendSwitch(
    sharedArrayBufferBlinkSwitch(startupConfig),
    "SharedArrayBufferUnrestrictedAccessAllowed",
);

registerCustomSchemes();

const invocation: ParsedInvocation | undefined = (() => {
    try {
        return parseInvocation(
            process.argv.slice(1),
            process.defaultApp ? app.getAppPath() : undefined,
        );
    } catch (error) {
        console.error("Invalid vosulay options", error);
        app.exit(APP_EXIT_FAILURE);
    }
})();

if (invocation !== undefined) {
    const hasSingleInstanceLock = app.requestSingleInstanceLock();
    if (!hasSingleInstanceLock) {
        app.quit();
    } else if (invocation.command === undefined) {
        void start(invocation.options).catch((cause: unknown) => {
            console.error("Failed to start vosulay", cause);
            app.exit(APP_EXIT_FAILURE);
        });
    } else {
        console.error("vosulay is not running");
        app.exit(APP_EXIT_FAILURE);
    }
}

async function start(options: RuntimeOptions): Promise<void> {
    // Electron's Menu API types the argument as `Menu | null` (no `undefined` overload).
    // eslint-disable-next-line unicorn/no-null
    Menu.setApplicationMenu(null);
    app.on("window-all-closed", () => {});
    const configStore = new ConfigStore(storagePaths.configPath);
    const overlayRegistry = new LocalOverlayRegistry(localOverlayCapabilityKey);
    const provider: OverlayProvider = await createOverlayProvider(
        process.platform,
        options,
        configStore,
        overlayRegistry,
    );
    let editShortcutRegistered = false;
    let remoteServer: RemoteOverlayServer | undefined = undefined;
    let shuttingDown = false;
    let tray: Tray | undefined = undefined;
    let unsubscribeRemoteConfig: (() => void) | undefined = undefined;
    let visibilityShortcutRegistered = false;

    app.on("second-instance", (_event, commandLine) => {
        let parseFailed = false;
        let command: ParsedInvocation["command"] = "toggle-overlay";
        try {
            ({ command } = parseInvocation(
                commandLine.slice(1),
                process.defaultApp ? app.getAppPath() : undefined,
            ));
        } catch (error) {
            parseFailed = true;
            console.error("Invalid vosulay second-instance options", error);
        }
        if (parseFailed) {
            return;
        }
        if (command === "toggle-overlay") {
            void provider.toggleVisibility().catch((cause: unknown) => {
                console.error("Failed to toggle overlay visibility", cause);
            });
        } else {
            void provider.reloadAll();
        }
    });
    app.on("before-quit", (event) => {
        event.preventDefault();
        if (shuttingDown) {
            return;
        }
        shuttingDown = true;
        if (editShortcutRegistered) {
            globalShortcut.unregister(EDIT_ACCELERATOR);
            editShortcutRegistered = false;
        }
        if (visibilityShortcutRegistered) {
            globalShortcut.unregister(VISIBILITY_ACCELERATOR);
            visibilityShortcutRegistered = false;
        }
        void (async () => {
            const failures: unknown[] = [];
            try {
                unsubscribeRemoteConfig?.();
                unsubscribeRemoteConfig = undefined;
                await remoteServer?.stop();
            } catch (error) {
                failures.push(error);
            }
            try {
                await provider.flushAll();
            } catch (error) {
                failures.push(error);
            }
            try {
                await configStore.flush();
            } catch (error) {
                failures.push(error);
            }
            try {
                await provider.stop();
            } catch (error) {
                failures.push(error);
            }
            try {
                tray?.destroy();
            } catch (error) {
                failures.push(error);
            }
            tray = undefined;
            if (failures.length > 0) {
                console.error("Failed to shut down vosulay cleanly", ...failures);
                app.exit(APP_EXIT_FAILURE);
                return;
            }
            app.exit(APP_EXIT_SUCCESS);
        })();
    });

    await app.whenReady();

    installCustomProtocols(options, overlayRegistry);
    const loadedConfig = await configStore.load();
    const initialConfig = loadedConfig.config;
    remoteServer = new RemoteOverlayServer({
        initialConfig,
        registry: overlayRegistry,
        runtime: options,
    });
    unsubscribeRemoteConfig = configStore.subscribe((change) => {
        remoteServer?.updateConfig(change.config);
    });
    try {
        await remoteServer.start();
    } catch (error) {
        console.error("Could not start remote overlay server on 0.0.0.0:5050", error);
    }
    session.defaultSession.setPermissionRequestHandler((_webContents, _permission, callback) =>
        callback(false),
    );
    session.defaultSession.setPermissionCheckHandler(() => false);
    const trayIcon = nativeImage.createFromBuffer(createTrayPixels(), {
        height: TRAY_ICON_SIZE,
        width: TRAY_ICON_SIZE,
    });
    tray = new Tray(trayIcon);
    tray.setToolTip("vosulay");

    const toggleEditing = (): void => {
        void provider.toggleEditing().catch((cause: unknown) => {
            console.error("Failed to toggle overlay edit mode", cause);
        });
    };
    const toggleVisibility = (): void => {
        void provider.toggleVisibility().catch((cause: unknown) => {
            console.error("Failed to toggle overlay visibility", cause);
        });
    };
    const editItem: MenuItemConstructorOptions = {
        checked: false,
        click: toggleEditing,
        label: "Edit overlays",
        type: "checkbox",
    };
    const trayMenu = Menu.buildFromTemplate([
        {
            ...editItem,
            id: "edit-overlays",
        },
        {
            click: toggleVisibility,
            id: "toggle-visibility",
            label: "Hide overlays",
        },
        {
            click: () => {
                void provider.reloadAll();
            },
            label: "Reload overlays",
        },
        { type: "separator" },
        {
            click: () => app.quit(),
            label: "Exit",
        },
    ]);
    tray.setContextMenu(trayMenu);

    const editMenuItem = trayMenu.getMenuItemById("edit-overlays") ?? undefined;
    const visibilityMenuItem = trayMenu.getMenuItemById("toggle-visibility") ?? undefined;
    provider.events.on("editingChanged", (editing) => {
        if (editMenuItem !== undefined) {
            editMenuItem.checked = editing;
        }
    });
    provider.events.on("visibilityChanged", (visible) => {
        if (visibilityMenuItem !== undefined) {
            visibilityMenuItem.label = visible ? "Hide overlays" : "Show overlays";
        }
    });
    editShortcutRegistered = globalShortcut.register(EDIT_ACCELERATOR, toggleEditing);
    if (!editShortcutRegistered) {
        console.warn(`Could not register ${EDIT_ACCELERATOR}; use the tray menu to edit overlays`);
    }
    visibilityShortcutRegistered = globalShortcut.register(
        VISIBILITY_ACCELERATOR,
        toggleVisibility,
    );
    if (!visibilityShortcutRegistered) {
        console.warn(
            `Could not register ${VISIBILITY_ACCELERATOR}; use the tray menu to show or hide overlays`,
        );
    }

    provider.start();
}

function createTrayPixels(): Buffer {
    const pixels = Buffer.alloc(TRAY_ICON_SIZE * TRAY_ICON_SIZE * TRAY_BUFFER_SIZE_BYTES);
    for (let y = TRAY_LOOP_PADDING; y < TRAY_LOOP_LIMIT; y += 1) {
        for (let x = TRAY_LOOP_PADDING; x < TRAY_LOOP_LIMIT; x += 1) {
            const offset = (y * TRAY_ICON_SIZE + x) * TRAY_BUFFER_SIZE_BYTES;
            const ring =
                x < TRAY_RING_WIDTH ||
                x >= TRAY_ICON_SIZE - TRAY_RING_WIDTH ||
                y < TRAY_RING_WIDTH ||
                y >= TRAY_ICON_SIZE - TRAY_RING_WIDTH;
            pixels[offset + TRAY_RED_CHANNEL] = ring ? TRAY_OUTER_RED : TRAY_OPAQUE_ALPHA;
            pixels[offset + TRAY_GREEN_CHANNEL] = ring ? TRAY_RING_GREEN : TRAY_OPAQUE_ALPHA;
            pixels[offset + TRAY_BLUE_CHANNEL] = ring ? TRAY_RING_BLUE : TRAY_OPAQUE_ALPHA;
            pixels[offset + TRAY_ALPHA_CHANNEL] = TRAY_OPAQUE_ALPHA;
        }
    }
    return pixels;
}
