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

import { ConfigStore } from './config';
import { resolveStoragePaths } from './storage-path';
import {
    LocalOverlayRegistry,
    installCustomProtocols,
    registerCustomSchemes
} from './local-overlays';
import { OverlayManager } from './overlay-manager';
import { parseOptions } from './options';

const EDIT_ACCELERATOR = 'Control+Shift+Space';

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

registerCustomSchemes();

const hasSingleInstanceLock = app.requestSingleInstanceLock();
if (!hasSingleInstanceLock) {
    app.quit();
} else {
    void start().catch((error: unknown) => {
        console.error('Failed to start vosulay', error);
        app.exit(1);
    });
}

async function start(): Promise<void> {
    let options;
    try {
        options = parseOptions(process.argv.slice(1));
    } catch (error) {
        console.error('Invalid vosulay options', error);
        app.exit(1);
        return;
    }

    Menu.setApplicationMenu(null);
    app.on('window-all-closed', () => {});

    const configStore = new ConfigStore(storagePaths.configPath);
    const overlayRegistry = new LocalOverlayRegistry();
    const manager = new OverlayManager(options, configStore, overlayRegistry);
    let tray: Tray | null = null;
    let shuttingDown = false;
    let editShortcutRegistered = false;

    app.on('second-instance', () => {
        void manager.reloadAll();
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
        void (async () => {
            const failures: unknown[] = [];
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
    manager.events.on('editingChanged', (editing) => {
        if (editMenuItem !== null) {
            editMenuItem.checked = editing;
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
