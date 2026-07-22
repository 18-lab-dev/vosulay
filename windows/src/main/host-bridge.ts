import { createRequire } from "node:module";
import { extname, basename, dirname, join } from "node:path";
import { readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { ConfigConflictError, type ConfigStore } from "./config";
import { fetchInstalledCounters } from "./counters";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import type { EditingTarget, MessageEnvelope, WebContentsLike } from "./types";
import { availableUrl, settingsUrl } from "./urls";

const require = createRequire(import.meta.url);
const IPC_CHANNEL = "vosulay:message";
const MAX_PROFILE_BYTES = 1024 * 1024;

interface IpcEventLike {
    sender: { id: number };
}

interface IpcMainLike {
    on(channel: string, listener: (event: IpcEventLike, payload: unknown) => void): void;
    removeListener(channel: string, listener: (event: IpcEventLike, payload: unknown) => void): void;
}

interface ElectronServices {
    ipcMain: IpcMainLike;
    dialog: {
        showOpenDialog(options: unknown): Promise<{ canceled: boolean; filePaths: string[] }>;
        showSaveDialog(options: unknown): Promise<{ canceled: boolean; filePath?: string }>;
    };
    shell: { openExternal(url: string): Promise<void> };
    app: { quit(): void };
}

function electron(): ElectronServices {
    return require("electron") as ElectronServices;
}

function objectMessage(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid vosulay manager message");
    return value as Record<string, unknown>;
}

export function localOverlayUrls(profileOrConfig: unknown): string[] {
    const object = objectMessage(profileOrConfig);
    const profiles = Array.isArray(object.profiles) ? object.profiles : [object];
    const urls: string[] = [];
    for (const profileValue of profiles) {
        const profile = objectMessage(profileValue);
        if (!Array.isArray(profile.overlays)) throw new Error("Imported profile must contain a valid overlays array");
        for (const overlayValue of profile.overlays) {
            const overlay = objectMessage(overlayValue);
            if (typeof overlay.url !== "string") continue;
            const value = overlay.url.trim();
            try {
                if (new URL(value).protocol.toLowerCase() === "vosulay-overlay:") urls.push(value);
            } catch {
                // Non-local invalid URLs are discarded by configuration normalization.
            }
        }
    }
    return urls;
}

export class HostBridge {
    private started = false;
    private initialized = false;
    private revision = 0;
    private readonly source = Symbol("HostBridge");
    private unsubscribeConfig: (() => void) | undefined;
    private dispatchTail: Promise<void> = Promise.resolve();
    private saveTail: Promise<void> = Promise.resolve();
    private flushSequence = 0;
    private readonly flushWaiters = new Map<string, {
        resolve: () => void;
        reject: (error: Error) => void;
        timeout: NodeJS.Timeout;
    }>();
    private readonly listener = (event: IpcEventLike, payload: unknown): void => {
        if (this.webContents.id !== undefined && event.sender.id !== this.webContents.id) return;
        if (this.trySettleFlush(payload)) return;
        if (this.messageType(payload) === "save-config") {
            const operation = this.saveTail.then(() => this.dispatch(payload));
            this.saveTail = operation.catch((error: unknown) => this.reportError(error));
            return;
        }
        const operation = this.dispatchTail.then(() => this.dispatch(payload));
        this.dispatchTail = operation.catch((error: unknown) => this.reportError(error));
    };

    constructor(
        private readonly webContents: WebContentsLike,
        private readonly editingTarget: EditingTarget,
        private readonly options: RuntimeOptions,
        private readonly configStore: ConfigStore,
        private readonly overlayRegistry: LocalOverlayRegistry,
        private readonly ipcMain: IpcMainLike = electron().ipcMain,
    ) {}

    start(): void {
        if (this.started) return;
        this.ipcMain.on(IPC_CHANNEL, this.listener);
        this.unsubscribeConfig = this.configStore.subscribe((change) => {
            this.revision = change.revision;
            if (change.source === this.source || !this.initialized) return;
            void this.send({ type: "config", config: change.config, revision: change.revision }).catch(() => undefined);
        });
        this.started = true;
    }

    async send(envelope: MessageEnvelope): Promise<void> {
        if (this.webContents.isDestroyed?.()) return;
        this.webContents.send(IPC_CHANNEL, envelope);
    }
    requestSaveFlush(timeoutMs = 5_000): Promise<void> {
        if (this.webContents.isDestroyed?.()) {
            return Promise.reject(new Error("Cannot flush a destroyed vosulay manager"));
        }
        const requestId = `flush-${this.webContents.id ?? 0}-${++this.flushSequence}`;
        return new Promise<void>((resolve, reject) => {
            const timeout = setTimeout(() => {
                this.flushWaiters.delete(requestId);
                reject(new Error("Timed out waiting for the vosulay manager to save"));
            }, timeoutMs);
            this.flushWaiters.set(requestId, { resolve, reject, timeout });
            void this.send({ type: "flush-save", request_id: requestId }).catch((error: unknown) => {
                const waiter = this.flushWaiters.get(requestId);
                if (!waiter) return;
                clearTimeout(waiter.timeout);
                this.flushWaiters.delete(requestId);
                waiter.reject(error instanceof Error ? error : new Error(String(error)));
            });
        });
    }


    dispose(): void {
        for (const [requestId, waiter] of this.flushWaiters) {
            clearTimeout(waiter.timeout);
            waiter.reject(new Error("vosulay manager closed before saving completed"));
            this.flushWaiters.delete(requestId);
        }
        if (!this.started) return;
        this.ipcMain.removeListener(IPC_CHANNEL, this.listener);
        this.unsubscribeConfig?.();
        this.unsubscribeConfig = undefined;
        this.initialized = false;
        this.started = false;
    }

    private async dispatch(payload: unknown): Promise<void> {
        const decoded = typeof payload === "string" ? JSON.parse(payload) as unknown : payload;
        const request = objectMessage(decoded);
        if (typeof request.type !== "string") throw new Error("Invalid vosulay manager message");
        switch (request.type) {
            case "ready":
                await this.initialize();
                return;
            case "save-config": {
                if (typeof request.save_id !== "string" || !request.save_id) throw new Error("Invalid vosulay save ID");
                if (!Number.isSafeInteger(request.base_revision) || (request.base_revision as number) < 0) {
                    throw new Error("Invalid vosulay configuration revision");
                }
                try {
                    const urls = localOverlayUrls(request.config);
                    if (!this.overlayRegistry.authorizeUrls(urls)) {
                        throw new Error("Configuration contains local overlay that could not be authorized");
                    }
                    const saved = await this.configStore.save(request.config, {
                        expectedRevision: request.base_revision as number,
                        source: this.source,
                    });
                    this.revision = saved.revision;
                    await this.send({ type: "saved", save_id: request.save_id, revision: saved.revision });
                } catch (error) {
                    const loaded = await this.configStore.load();
                    await this.send({
                        type: "save-error",
                        save_id: request.save_id,
                        message: error instanceof Error ? error.message : String(error),
                        code: error instanceof ConfigConflictError ? "conflict" : "save",
                        config: loaded.config,
                        revision: loaded.revision,
                    });
                }
                return;
            }
            case "refresh-counters":
                await this.refreshCounters();
                return;
            case "set-editing":
                await this.editingTarget.setEditing(Boolean(request.editing));
                await this.send({ type: "editing", editing: this.editingTarget.editing });
                return;
            case "editing-applied":
                return;
            case "choose-overlay-file":
                await this.chooseOverlayFile();
                return;
            case "import-profile":
                await this.importProfile();
                return;
            case "export-profile":
                await this.exportProfile(request.profile);
                return;
            case "authorize-profile-import":
                await this.authorizeProfile(request.profile);
                return;
            case "open-settings":
                if (typeof request.counter_id !== "string") throw new Error("Counter identifier is required");
                await electron().shell.openExternal(settingsUrl(this.options.vosuUrl, request.counter_id).toString());
                return;
            case "open-available":
                await electron().shell.openExternal(availableUrl(this.options.vosuUrl).toString());
                return;
            case "quit":
                await this.configStore.flush();
                electron().app.quit();
                return;
            default:
                throw new Error("Unknown vosulay manager message");
        }
    }

    private async initialize(): Promise<void> {
        const loaded = await this.configStore.load();
        this.revision = loaded.revision;
        this.initialized = true;
        const urls = localOverlayUrls(loaded.config);
        if (!this.overlayRegistry.authorizeUrls(urls)) loaded.error = "Could not authorize one or more local overlays";
        await this.send({ type: "config", config: loaded.config, revision: loaded.revision });
        if (loaded.error) await this.send({ type: "error", message: loaded.error });
        await this.send({ type: "editing", editing: this.editingTarget.editing });
        await this.refreshCounters();
    }

    private messageType(payload: unknown): string | null {
        try {
            const decoded = typeof payload === "string" ? JSON.parse(payload) as unknown : payload;
            if (!decoded || typeof decoded !== "object" || Array.isArray(decoded)) return null;
            const type = (decoded as Record<string, unknown>).type;
            return typeof type === "string" ? type : null;
        } catch {
            return null;
        }
    }

    private trySettleFlush(payload: unknown): boolean {
        let decoded: unknown;
        try {
            decoded = typeof payload === "string" ? JSON.parse(payload) as unknown : payload;
        } catch {
            return false;
        }
        if (!decoded || typeof decoded !== "object" || Array.isArray(decoded)) return false;
        const request = decoded as Record<string, unknown>;
        if (request.type !== "flush-complete" && request.type !== "flush-failed") return false;

        try {
            this.settleFlush(
                request.request_id,
                request.type === "flush-failed"
                    ? new Error(typeof request.message === "string" ? request.message : "vosulay manager could not save")
                    : null,
            );
        } catch (error) {
            void this.reportError(error);
        }
        return true;
    }

    private settleFlush(requestId: unknown, error: Error | null): void {
        if (typeof requestId !== "string") throw new Error("Invalid vosulay flush request ID");
        const waiter = this.flushWaiters.get(requestId);
        if (!waiter) return;
        clearTimeout(waiter.timeout);
        this.flushWaiters.delete(requestId);
        if (error) waiter.reject(error);
        else waiter.resolve();
    }

    private async reportError(error: unknown): Promise<void> {
        try {
            await this.send({ type: "error", message: error instanceof Error ? error.message : String(error) });
        } catch (sendError) {
            console.error("Failed to report vosulay manager error", sendError);
        }
    }

    private async refreshCounters(): Promise<void> {
        const counters = await fetchInstalledCounters(this.options.vosuUrl);
        await this.send({ type: "counters", counters });
    }

    private async chooseOverlayFile(): Promise<void> {
        const result = await electron().dialog.showOpenDialog({
            title: "Choose HTML overlay",
            properties: ["openFile"],
            filters: [{ name: "HTML files", extensions: ["html", "htm"] }],
        });
        if (result.canceled || !result.filePaths[0]) return;
        const path = result.filePaths[0];
        if (![".html", ".htm"].includes(extname(path).toLowerCase()) || !(await stat(path)).isFile()) {
            throw new Error("Selected overlay is not a readable HTML file");
        }
        const url = await this.overlayRegistry.createUrl(path);
        await this.send({ type: "overlay-file", url, name: basename(path, extname(path)) });
    }

    private async importProfile(): Promise<void> {
        const result = await electron().dialog.showOpenDialog({
            title: "Import profile",
            properties: ["openFile"],
            filters: [{ name: "JSON profile files", extensions: ["json"] }],
        });
        if (result.canceled || !result.filePaths[0]) return;
        const path = result.filePaths[0];
        const info = await stat(path);
        if (!info.isFile() || extname(path).toLowerCase() !== ".json") throw new Error("Selected profile is not a readable JSON file");
        if (info.size > MAX_PROFILE_BYTES) throw new Error("Selected profile is too large (maximum 1 MiB)");
        const profile = JSON.parse(await readFile(path, "utf8")) as unknown;
        const object = objectMessage(profile);
        localOverlayUrls(object);
        await this.send({ type: "profile-import", profile: object });
    }

    private async authorizeProfile(profile: unknown): Promise<void> {
        const object = objectMessage(profile);
        const urls = localOverlayUrls(object);
        if (!this.overlayRegistry.authorizeUrls(urls)) throw new Error("Imported profile contains local overlay that could not be authorized");
        await this.send({ type: "profile-import-authorized", profile: object });
    }

    private async exportProfile(profile: unknown): Promise<void> {
        const object = objectMessage(profile);
        localOverlayUrls(object);
        const name = typeof object.name === "string" && object.name.trim() ? object.name.trim() : "profile";
        const safeName = name.replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_");
        const result = await electron().dialog.showSaveDialog({
            title: "Export profile",
            defaultPath: `${safeName}.json`,
            filters: [{ name: "JSON profile files", extensions: ["json"] }],
        });
        if (result.canceled || !result.filePath) return;
        if (extname(result.filePath).toLowerCase() !== ".json") throw new Error("The export filename must end with .json");
        const temporary = join(dirname(result.filePath), `.${process.pid}-${Date.now()}.tmp`);
        try {
            await writeFile(temporary, `${JSON.stringify(object, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
            await rename(temporary, result.filePath);
        } catch (error) {
            await rm(temporary, { force: true });
            throw error;
        }
        await this.send({ type: "profile-exported" });
    }
}
