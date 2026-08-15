import {
    Menu,
    type MenuItemConstructorOptions,
    Tray,
    app,
    globalShortcut,
    nativeImage,
    session,
} from "electron";
import { mkdirSync } from "node:fs";

import { ConfigStore, loadConfigSync, sharedArrayBufferBlinkSwitch } from "./config";
import { resolveStoragePaths } from "./storage-path";
import {
    LocalOverlayRegistry,
    installCustomProtocols,
    loadLocalOverlayCapabilityKey,
    registerCustomSchemes,
} from "./local-overlays";
import { createOverlayProvider } from "./provider-factory";
import type { OverlayProvider } from "./overlay-provider";
import { RemoteOverlayServer } from "./remote-server";
import { parseInvocation } from "./options";
import type { ParsedInvocation, RuntimeOptions } from "./options";

const EDIT_ACCELERATOR = "Control+Shift+Space",
    VISIBILITY_ACCELERATOR = "Control+Alt+Shift+H";
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

let invocation;
try {
    invocation = parseInvocation(
        process.argv.slice(1),
        process.defaultApp ? app.getAppPath() : undefined,
    );
} catch (error) {
    console.error("Invalid vosulay options", error);
    app.exit(1);
}

if (invocation !== undefined) {
    const hasSingleInstanceLock = app.requestSingleInstanceLock();
    if (!hasSingleInstanceLock) {
        app.quit();
    } else if (invocation.command === undefined) {
        void start(invocation.options).catch((error: unknown) => {
            console.error("Failed to start vosulay", error);
            app.exit(1);
        });
    } else {
        console.error("vosulay is not running");
        app.exit(1);
    }
}

async function start(options: RuntimeOptions): Promise<void> {
    // Electron's Menu API types the argument as `Menu | null` (no `undefined` overload).
    // eslint-disable-next-line unicorn/no-null
    Menu.setApplicationMenu(null);
    app.on("window-all-closed", () => {});
    const configStore = new ConfigStore(storagePaths.configPath),
        overlayRegistry = new LocalOverlayRegistry(localOverlayCapabilityKey),
        provider: OverlayProvider = await createOverlayProvider(
            process.platform,
            options,
            configStore,
            overlayRegistry,
        );
    let editShortcutRegistered = false,
        remoteServer: RemoteOverlayServer | undefined = undefined,
        shuttingDown = false,
        tray: Tray | undefined = undefined,
        unsubscribeRemoteConfig: (() => void) | undefined = undefined,
        visibilityShortcutRegistered = false;

    app.on("second-instance", (_event, commandLine) => {
        let command;
        try {
            command = parseInvocation(
                commandLine.slice(1),
                process.defaultApp ? app.getAppPath() : undefined,
            ).command;
        } catch (error) {
            console.error("Invalid vosulay second-instance options", error);
            return;
        }
        if (command === "toggle-overlay") {
            void provider.toggleVisibility().catch((error: unknown) => {
                console.error("Failed to toggle overlay visibility", error);
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
                app.exit(1);
                return;
            }
            app.exit(0);
        })();
    });

    await app.whenReady();

    await Promise.resolve(installCustomProtocols(options, overlayRegistry));
    const initialConfig = (await configStore.load()).config;
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

    const trayIcon = nativeImage.createFromBuffer(createTrayPixels(), { height: 16, width: 16 });
    tray = new Tray(trayIcon);
    tray.setToolTip("vosulay");

    const toggleEditing = (): void => {
            void provider.toggleEditing().catch((error: unknown) => {
                console.error("Failed to toggle overlay edit mode", error);
            });
        },
        toggleVisibility = (): void => {
            void provider.toggleVisibility().catch((error: unknown) => {
                console.error("Failed to toggle overlay visibility", error);
            });
        },
        editItem: MenuItemConstructorOptions = {
            checked: false,
            click: toggleEditing,
            label: "Edit overlays",
            type: "checkbox",
        },
        trayMenu = Menu.buildFromTemplate([
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

    const editMenuItem = trayMenu.getMenuItemById("edit-overlays") ?? undefined,
        visibilityMenuItem = trayMenu.getMenuItemById("toggle-visibility") ?? undefined;
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
    const pixels = Buffer.alloc(16 * 16 * 4);
    for (let y = 1; y < 15; y += 1) {
        for (let x = 1; x < 15; x += 1) {
            const offset = (y * 16 + x) * 4,
                ring = x < 4 || x > 11 || y < 4 || y > 11;
            pixels[offset] = ring ? 233 : 255;
            pixels[offset + 1] = ring ? 116 : 255;
            pixels[offset + 2] = ring ? 165 : 255;
            pixels[offset + 3] = 255;
        }
    }
    return pixels;
}
