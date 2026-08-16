import { ConfigConflictError, type ConfigStore } from "./config";
import type { EditingTarget, MessageEnvelope, WebContentsLike } from "./types";
import {
    type JsonObject,
    type JsonValue,
    isJsonArray,
    isJsonNumber,
    isJsonObject,
    isJsonString,
    parseJsonText,
} from "./json";
import { availableUrl, settingsUrl } from "./urls";
import { basename, dirname, extname, join } from "node:path";
import { readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import { createRequire } from "node:module";
import { fetchInstalledCounters } from "./counters";

const require = createRequire(import.meta.url);
const IPC_CHANNEL = "vosulay:message";
const BYTES_PER_KIB = 1024;
const MAX_PROFILE_BYTES = BYTES_PER_KIB * BYTES_PER_KIB;
const PROFILE_JSON_INDENT = 2;
const PROFILE_FILE_MODE = 0o600;
const SAVE_FLUSH_TIMEOUT_MS = 5000;
const LAST_CONTROL_CODE_POINT = 0x1f;
const RESERVED_NAME_CHARACTERS = new Set(["<", ">", ":", '"', "/", "\\", "|", "?", "*"]);

interface IpcEventLike {
    sender: { id: number };
}

type IpcMessageListener = (event: IpcEventLike, payload: JsonValue) => void;

interface IpcMainLike {
    on: (channel: string, listener: IpcMessageListener) => void;
    removeListener: (channel: string, listener: IpcMessageListener) => void;
}

/** The subset of Electron's dialog options this host passes through. */
interface FileDialogOptions {
    title: string;
    defaultPath?: string;
    properties?: readonly string[];
    filters: readonly { name: string; extensions: readonly string[] }[];
}

interface ElectronServices {
    ipcMain: IpcMainLike;
    dialog: {
        showOpenDialog: (
            options: FileDialogOptions,
        ) => Promise<{ canceled: boolean; filePaths: string[] }>;
        showSaveDialog: (
            options: FileDialogOptions,
        ) => Promise<{ canceled: boolean; filePath?: string }>;
    };
    shell: { openExternal: (url: string) => Promise<void> };
    app: { quit: () => void };
}

function electron(): ElectronServices {
    /* SAFETY: Electron's main-process module always exposes these services; the host only runs inside Electron. */
    return require("electron") as ElectronServices;
}

function objectMessage(value: JsonValue | undefined): JsonObject {
    if (!isJsonObject(value)) {
        throw new Error("Invalid vosulay manager message");
    }
    return value;
}

/*
 * Strips the characters no common filesystem accepts in a name, matching the
 * exact set the exported profile filename has always rejected: the reserved
 * punctuation plus the C0 control range. It is written as a scan rather than a
 * regex because a `u`-flagged pattern cannot spell a control range literally,
 * and `\p{Cc}` would also swallow the C1 controls this never touched.
 */
function sanitizeProfileName(name: string): string {
    return [...name]
        .map((character) => {
            const code = character.codePointAt(0) ?? 0;
            return RESERVED_NAME_CHARACTERS.has(character) || code <= LAST_CONTROL_CODE_POINT
                ? "_"
                : character;
        })
        .join("");
}

export function localOverlayUrls(profileOrConfig: JsonValue | undefined): string[] {
    const object = objectMessage(profileOrConfig);
    const profiles = isJsonArray(object.profiles) ? object.profiles : [object];
    const urls: string[] = [];
    for (const profileValue of profiles) {
        const profile = objectMessage(profileValue);
        if (!isJsonArray(profile.overlays)) {
            throw new TypeError("Imported profile must contain a valid overlays array");
        }
        for (const overlayValue of profile.overlays) {
            const overlay = objectMessage(overlayValue);
            if (!isJsonString(overlay.url)) {
                continue;
            }
            const value = overlay.url.trim();
            try {
                if (new URL(value).protocol.toLowerCase() === "vosulay-overlay:") {
                    urls.push(value);
                }
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
    private readonly flushWaiters = new Map<
        string,
        {
            resolve: () => void;
            reject: (error: Error) => void;
            timeout: NodeJS.Timeout;
        }
    >();
    private readonly listener: IpcMessageListener = (event, payload): void => {
        if (this.webContents.id !== undefined && event.sender.id !== this.webContents.id) {
            return;
        }
        if (this.trySettleFlush(payload)) {
            return;
        }
        if (this.messageType(payload) === "save-config") {
            const operation = this.saveTail.then(() => this.dispatch(payload));
            this.saveTail = operation.catch((cause: unknown) => this.reportError(cause));
            return;
        }
        const operation = this.dispatchTail.then(() => this.dispatch(payload));
        this.dispatchTail = operation.catch((cause: unknown) => this.reportError(cause));
    };
    private readonly webContents: WebContentsLike;
    private readonly editingTarget: EditingTarget;
    private readonly options: RuntimeOptions;
    private readonly configStore: ConfigStore;
    private readonly overlayRegistry: LocalOverlayRegistry;
    private readonly ipcMain: IpcMainLike;

    constructor(
        webContents: WebContentsLike,
        editingTarget: EditingTarget,
        options: RuntimeOptions,
        configStore: ConfigStore,
        overlayRegistry: LocalOverlayRegistry,
        ipcMain: IpcMainLike = electron().ipcMain,
    ) {
        this.webContents = webContents;
        this.editingTarget = editingTarget;
        this.options = options;
        this.configStore = configStore;
        this.overlayRegistry = overlayRegistry;
        this.ipcMain = ipcMain;
    }

    start(): void {
        if (this.started) {
            return;
        }
        this.ipcMain.on(IPC_CHANNEL, this.listener);
        this.unsubscribeConfig = this.configStore.subscribe((change) => {
            this.revision = change.revision;
            if (change.source === this.source || !this.initialized) {
                return;
            }
            void this.send({
                config: change.config,
                revision: change.revision,
                type: "config",
            }).catch(() => {});
        });
        this.started = true;
    }

    send(envelope: MessageEnvelope): Promise<void> {
        if (this.webContents.isDestroyed?.()) {
            return Promise.resolve();
        }
        try {
            this.webContents.send(IPC_CHANNEL, envelope);
        } catch (cause: unknown) {
            return Promise.reject(cause instanceof Error ? cause : new Error(String(cause)));
        }
        return Promise.resolve();
    }

    requestSaveFlush(timeoutMs = SAVE_FLUSH_TIMEOUT_MS): Promise<void> {
        if (this.webContents.isDestroyed?.()) {
            return Promise.reject(new Error("Cannot flush a destroyed vosulay manager"));
        }
        const requestId = `flush-${this.webContents.id ?? 0}-${++this.flushSequence}`;
        return new Promise<void>((resolve, reject) => {
            const timeout = setTimeout(() => {
                this.flushWaiters.delete(requestId);
                reject(new Error("Timed out waiting for the vosulay manager to save"));
            }, timeoutMs);
            this.flushWaiters.set(requestId, { reject, resolve, timeout });
            void this.send({ request_id: requestId, type: "flush-save" }).catch(
                (cause: unknown) => {
                    const waiter = this.flushWaiters.get(requestId);
                    if (!waiter) {
                        return;
                    }
                    clearTimeout(waiter.timeout);
                    this.flushWaiters.delete(requestId);
                    waiter.reject(cause instanceof Error ? cause : new Error(String(cause)));
                },
            );
        });
    }

    dispose(): void {
        for (const [requestId, waiter] of this.flushWaiters) {
            clearTimeout(waiter.timeout);
            waiter.reject(new Error("vosulay manager closed before saving completed"));
            this.flushWaiters.delete(requestId);
        }
        if (!this.started) {
            return;
        }
        this.ipcMain.removeListener(IPC_CHANNEL, this.listener);
        this.unsubscribeConfig?.();
        this.unsubscribeConfig = undefined;
        this.initialized = false;
        this.started = false;
    }

    /** Decode one manager message, whether it arrived as JSON text or as a structured payload. */
    private decodeRequest(payload: JsonValue): JsonObject {
        return objectMessage(isJsonString(payload) ? parseJsonText(payload) : payload);
    }

    private async dispatch(payload: JsonValue): Promise<void> {
        const request = this.decodeRequest(payload);
        if (!isJsonString(request.type)) {
            throw new TypeError("Invalid vosulay manager message");
        }
        switch (request.type) {
            case "ready": {
                await this.initialize();
                break;
            }
            case "save-config": {
                const saveId = request.save_id;
                const baseRevision = request.base_revision;
                if (!isJsonString(saveId) || !saveId) {
                    throw new Error("Invalid vosulay save ID");
                }
                if (
                    !isJsonNumber(baseRevision) ||
                    !Number.isSafeInteger(baseRevision) ||
                    baseRevision < 0
                ) {
                    throw new Error("Invalid vosulay configuration revision");
                }
                try {
                    const urls = localOverlayUrls(request.config);
                    if (!this.overlayRegistry.authorizeUrls(urls)) {
                        throw new Error(
                            "Configuration contains local overlay that could not be authorized",
                        );
                    }
                    const saved = await this.configStore.save(request.config, {
                        expectedRevision: baseRevision,
                        source: this.source,
                    });
                    this.revision = saved.revision;
                    await this.send({
                        revision: saved.revision,
                        save_id: saveId,
                        type: "saved",
                    });
                } catch (error) {
                    const loaded = await this.configStore.load();
                    await this.send({
                        code: error instanceof ConfigConflictError ? "conflict" : "save",
                        config: loaded.config,
                        message: error instanceof Error ? error.message : String(error),
                        revision: loaded.revision,
                        save_id: saveId,
                        type: "save-error",
                    });
                }
                break;
            }
            case "refresh-counters": {
                await this.refreshCounters();
                break;
            }
            case "set-editing": {
                await this.editingTarget.setEditing(Boolean(request.editing));
                await this.send({ editing: this.editingTarget.editing, type: "editing" });
                break;
            }
            case "editing-applied": {
                break;
            }
            case "choose-overlay-file": {
                await this.chooseOverlayFile();
                break;
            }
            case "import-profile": {
                await this.importProfile();
                break;
            }
            case "export-profile": {
                await this.exportProfile(request.profile);
                break;
            }
            case "authorize-profile-import": {
                await this.authorizeProfile(request.profile);
                break;
            }
            case "open-settings": {
                const counterId = request.counter_id;
                if (!isJsonString(counterId)) {
                    throw new TypeError("Counter identifier required");
                }
                await electron().shell.openExternal(
                    settingsUrl(this.options.vosuUrl, counterId).toString(),
                );
                break;
            }
            case "open-available": {
                await electron().shell.openExternal(availableUrl(this.options.vosuUrl).toString());
                break;
            }
            case "quit": {
                await this.configStore.flush();
                electron().app.quit();
                break;
            }
            default: {
                throw new Error("Unknown vosulay manager message");
            }
        }
    }

    private async initialize(): Promise<void> {
        const loaded = await this.configStore.load();
        this.revision = loaded.revision;
        this.initialized = true;
        const urls = localOverlayUrls(loaded.config);
        if (!this.overlayRegistry.authorizeUrls(urls)) {
            loaded.error = "Could not authorize one or more local overlays";
        }
        await this.send({ config: loaded.config, revision: loaded.revision, type: "config" });
        if (loaded.error) {
            await this.send({ message: loaded.error, type: "error" });
        }
        await this.send({ editing: this.editingTarget.editing, type: "editing" });
        await this.refreshCounters();
    }

    private messageType(payload: JsonValue): string | undefined {
        const decoded = isJsonString(payload) ? parseJsonText(payload) : payload;
        if (!isJsonObject(decoded)) {
            return undefined;
        }
        const { type } = decoded;
        return isJsonString(type) ? type : undefined;
    }

    private trySettleFlush(payload: JsonValue): boolean {
        const decoded = isJsonString(payload) ? parseJsonText(payload) : payload;
        if (!isJsonObject(decoded)) {
            return false;
        }
        const request = decoded;
        if (request.type !== "flush-complete" && request.type !== "flush-failed") {
            return false;
        }

        try {
            const { message } = request;
            this.settleFlush(
                request.request_id,
                request.type === "flush-failed"
                    ? new Error(isJsonString(message) ? message : "vosulay manager could not save")
                    : undefined,
            );
        } catch (error) {
            void this.reportError(error);
        }
        return true;
    }

    private settleFlush(requestId: JsonValue | undefined, error: Error | undefined): void {
        if (!isJsonString(requestId)) {
            throw new TypeError("Invalid vosulay flush request ID");
        }
        const waiter = this.flushWaiters.get(requestId);
        if (!waiter) {
            return;
        }
        clearTimeout(waiter.timeout);
        this.flushWaiters.delete(requestId);
        if (error) {
            waiter.reject(error);
        } else {
            waiter.resolve();
        }
    }

    private async reportError(cause: unknown): Promise<void> {
        try {
            await this.send({
                message: cause instanceof Error ? cause.message : String(cause),
                type: "error",
            });
        } catch (sendError) {
            console.error("Failed to report vosulay manager error", sendError);
        }
    }

    private async refreshCounters(): Promise<void> {
        const counters = await fetchInstalledCounters(this.options.vosuUrl);
        await this.send({ counters, type: "counters" });
    }

    private async chooseOverlayFile(): Promise<void> {
        const result = await electron().dialog.showOpenDialog({
            filters: [{ extensions: ["html", "htm"], name: "HTML files" }],
            properties: ["openFile"],
            title: "Choose HTML overlay",
        });
        const [path] = result.filePaths;
        if (result.canceled || path === undefined || path === "") {
            return;
        }
        if (![".html", ".htm"].includes(extname(path).toLowerCase())) {
            throw new Error("Selected overlay is not a readable HTML file");
        }
        const info = await stat(path);
        if (!info.isFile()) {
            throw new Error("Selected overlay is not a readable HTML file");
        }
        const url = await this.overlayRegistry.createUrl(path);
        await this.send({ name: basename(path, extname(path)), type: "overlay-file", url });
    }

    private async importProfile(): Promise<void> {
        const result = await electron().dialog.showOpenDialog({
            filters: [{ extensions: ["json"], name: "JSON profile files" }],
            properties: ["openFile"],
            title: "Import profile",
        });
        const [path] = result.filePaths;
        if (result.canceled || path === undefined || path === "") {
            return;
        }
        const info = await stat(path);
        if (!info.isFile() || extname(path).toLowerCase() !== ".json") {
            throw new Error("Selected profile is not a readable JSON file");
        }
        if (info.size > MAX_PROFILE_BYTES) {
            throw new Error("Selected profile is too large (maximum 1 MiB)");
        }
        const profile = parseJsonText(await readFile(path, "utf8"));
        const object = objectMessage(profile);
        localOverlayUrls(object);
        await this.send({ profile: object, type: "profile-import" });
    }

    private async authorizeProfile(profile: JsonValue | undefined): Promise<void> {
        const object = objectMessage(profile);
        const urls = localOverlayUrls(object);
        if (!this.overlayRegistry.authorizeUrls(urls)) {
            throw new Error("Imported profile contains local overlay that could not be authorized");
        }
        await this.send({ profile: object, type: "profile-import-authorized" });
    }

    private async exportProfile(profile: JsonValue | undefined): Promise<void> {
        const object = objectMessage(profile);
        localOverlayUrls(object);
        const { name: rawName } = object;
        const name = isJsonString(rawName) && rawName.trim() ? rawName.trim() : "profile";
        const safeName = sanitizeProfileName(name);
        const result = await electron().dialog.showSaveDialog({
            defaultPath: `${safeName}.json`,
            filters: [{ extensions: ["json"], name: "JSON profile files" }],
            title: "Export profile",
        });
        if (result.canceled || !result.filePath) {
            return;
        }
        if (extname(result.filePath).toLowerCase() !== ".json") {
            throw new Error("The export filename must end with .json");
        }
        const temporary = join(dirname(result.filePath), `.${process.pid}-${Date.now()}.tmp`);
        try {
            await writeFile(
                temporary,
                `${JSON.stringify(object, undefined, PROFILE_JSON_INDENT)}\n`,
                {
                    encoding: "utf8",
                    flag: "wx",
                    mode: PROFILE_FILE_MODE,
                },
            );
            await rename(temporary, result.filePath);
        } catch (error) {
            await rm(temporary, { force: true });
            throw error;
        }
        await this.send({ type: "profile-exported" });
    }
}
