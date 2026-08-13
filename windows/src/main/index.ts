import {
    Menu,
    Tray,
    app,
    nativeImage,
    globalShortcut,
    session,
    type MenuItemConstructorOptions
} from 'electron';
import { mkdirSync } from 'node:fs';

import {
    ConfigStore,
    loadConfigSync,
    sharedArrayBufferBlinkSwitch
} from './config';
import { resolveStoragePaths } from './storage-path';
import {
    LocalOverlayRegistry,
    installCustomProtocols,
    registerCustomSchemes
} from './local-overlays';
import { OverlayManager } from './overlay-manager';
import { RemoteOverlayServer } from './remote-server';
import { parseInvocation } from './options';
import type { ParsedInvocation, RuntimeOptions } from './options';

const EDIT_ACCELERATOR = 'Control+Shift+Space';
const VISIBILITY_ACCELERATOR = 'Control+Alt+Shift+H';

app.commandLine.appendSwitch('force_high_performance_gpu');
app.commandLine.appendSwitch('high-dpi-support', '1');
app.commandLine.appendSwitch('force-device-scale-factor', '1');
app.commandLine.appendSwitch('in-process-gpu');
app.commandLine.appendSwitch('disable-direct-composition');

const storagePaths = resolveStoragePaths(
    app.getPath('appData'),
    process.env.PORTABLE_EXECUTABLE_DIR
);
mkdirSync(storagePaths.dataDirectory, { recursive: true });
app.setPath('userData', storagePaths.dataDirectory);
const startupConfig = loadConfigSync(storagePaths.configPath);
app.commandLine.appendSwitch(
    sharedArrayBufferBlinkSwitch(startupConfig),
    'SharedArrayBufferUnrestrictedAccessAllowed'
);

registerCustomSchemes();

let invocation: ParsedInvocation | null = null;
try {
    invocation = parseInvocation(process.argv.slice(1));
} catch (error) {
    console.error('Invalid vosulay options', error);
    app.exit(1);
}

if (invocation !== null) {
    const hasSingleInstanceLock = app.requestSingleInstanceLock();
    if (!hasSingleInstanceLock) {
        app.quit();
    } else if (invocation.command !== null) {
        console.error('vosulay is not running');
        app.exit(1);
    } else {
        void start(invocation.options).catch((error: unknown) => {
            console.error('Failed to start vosulay', error);
            app.exit(1);
        });
    }
}

async function start(options: RuntimeOptions): Promise<void> {
    Menu.setApplicationMenu(null);
    app.on('window-all-closed', () => {});

    const configStore = new ConfigStore(storagePaths.configPath);
    const overlayRegistry = new LocalOverlayRegistry();
    const manager = new OverlayManager(options, configStore, overlayRegistry);
    let remoteServer: RemoteOverlayServer | null = null;
    let unsubscribeRemoteConfig: (() => void) | null = null;
    let tray: Tray | null = null;
    let shuttingDown = false;
    let editShortcutRegistered = false;
    let visibilityShortcutRegistered = false;

    app.on('second-instance', (_event, commandLine) => {
        let command;
        try {
            command = parseInvocation(commandLine.slice(1)).command;
        } catch (error) {
            console.error('Invalid vosulay second-instance options', error);
            return;
        }
        if (command === 'toggle-overlay') {
            void manager.toggleVisibility().catch((error: unknown) => {
                console.error('Failed to toggle overlay visibility', error);
            });
        } else {
            void manager.reloadAll();
        }
    });
    app.on('before-quit', (event) => {
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
                unsubscribeRemoteConfig = null;
                await remoteServer?.stop();
            } catch (error) {
                failures.push(error);
            }
            try {
                await manager.flushAll();
            } catch (error) {
                failures.push(error);
            }
            try {
                await configStore.flush();
            } catch (error) {
                failures.push(error);
            }
            try {
                await manager.stop();
            } catch (error) {
                failures.push(error);
            }
            try {
                tray?.destroy();
            } catch (error) {
                failures.push(error);
            }
            tray = null;
            if (failures.length > 0) {
                console.error('Failed to shut down vosulay cleanly', ...failures);
                app.exit(1);
                return;
            }
            app.exit(0);
        })();
    });

    await app.whenReady();
    if (process.platform !== 'win32') {
        console.error('vosulay Windows runtime can only run on Windows');
        app.quit();
        return;
    }

    await Promise.resolve(installCustomProtocols(options, overlayRegistry));
    const initialConfig = (await configStore.load()).config;
    remoteServer = new RemoteOverlayServer({
        runtime: options,
        registry: overlayRegistry,
        initialConfig,
    });
    unsubscribeRemoteConfig = configStore.subscribe((change) => {
        remoteServer?.updateConfig(change.config);
    });
    try {
        await remoteServer.start();
    } catch (error) {
        console.error('Could not start remote overlay server on 0.0.0.0:5050', error);
    }
    session.defaultSession.setPermissionRequestHandler(
        (_webContents, _permission, callback) => callback(false)
    );
    session.defaultSession.setPermissionCheckHandler(() => false);

    const trayIcon = nativeImage.createFromBuffer(
        createTrayPixels(),
        { width: 16, height: 16 }
    );
    tray = new Tray(trayIcon);
    tray.setToolTip('vosulay');

    const toggleEditing = (): void => {
        void manager.toggleEditing().catch((error: unknown) => {
            console.error('Failed to toggle overlay edit mode', error);
        });
    };
    const toggleVisibility = (): void => {
        void manager.toggleVisibility().catch((error: unknown) => {
            console.error('Failed to toggle overlay visibility', error);
        });
    };
    const editItem: MenuItemConstructorOptions = {
        label: 'Edit overlays',
        type: 'checkbox',
        checked: false,
        click: toggleEditing
    };
    const trayMenu = Menu.buildFromTemplate([
        {
            ...editItem,
            id: 'edit-overlays'
        },
        {
            id: 'toggle-visibility',
            label: 'Hide overlays',
            click: toggleVisibility
        },
        {
            label: 'Reload overlays',
            click: () => {
                void manager.reloadAll();
            }
        },
        { type: 'separator' },
        {
            label: 'Exit',
            click: () => app.quit()
        }
    ]);
    tray.setContextMenu(trayMenu);

    const editMenuItem = trayMenu.getMenuItemById('edit-overlays');
    const visibilityMenuItem = trayMenu.getMenuItemById('toggle-visibility');
    manager.events.on('editingChanged', (editing) => {
        if (editMenuItem !== null) {
            editMenuItem.checked = editing;
        }
    });
    manager.events.on('visibilityChanged', (visible) => {
        if (visibilityMenuItem !== null) {
            visibilityMenuItem.label = visible ? 'Hide overlays' : 'Show overlays';
        }
    });
    editShortcutRegistered = globalShortcut.register(
        EDIT_ACCELERATOR,
        toggleEditing
    );
    if (!editShortcutRegistered) {
        console.warn(
            `Could not register ${EDIT_ACCELERATOR}; use the tray menu to edit overlays`
        );
    }
    visibilityShortcutRegistered = globalShortcut.register(
        VISIBILITY_ACCELERATOR,
        toggleVisibility
    );
    if (!visibilityShortcutRegistered) {
        console.warn(
            `Could not register ${VISIBILITY_ACCELERATOR}; use the tray menu to show or hide overlays`
        );
    }

    manager.start();
}

function createTrayPixels(): Buffer {
    const pixels = Buffer.alloc(16 * 16 * 4);
    for (let y = 1; y < 15; y += 1) {
        for (let x = 1; x < 15; x += 1) {
            const offset = (y * 16 + x) * 4;
            const ring = x < 4 || x > 11 || y < 4 || y > 11;
            pixels[offset] = ring ? 233 : 255;
            pixels[offset + 1] = ring ? 116 : 255;
            pixels[offset + 2] = ring ? 165 : 255;
            pixels[offset + 3] = 255;
        }
    }
    return pixels;
}
