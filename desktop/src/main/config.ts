import { constants, readFileSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { AppConfig, AppSettings, HotkeySettings, Placement, Profile } from "./types";

const CONFIG_VERSION = 1 as const,
    DEFAULT_ALLOWED_IPS = "127.0.0.1,localhost,absolute",
    REMOTE_ACCESS_TOKEN_BYTES = 32,
    DEFAULT_HOTKEYS: HotkeySettings = {
        copy_overlay: "Ctrl+C",
        paste_overlay: "Ctrl+V",
        profile_0: "Ctrl+0",
        profile_1: "Ctrl+1",
        profile_2: "Ctrl+2",
        profile_3: "Ctrl+3",
        profile_4: "Ctrl+4",
        profile_5: "Ctrl+5",
        profile_6: "Ctrl+6",
        profile_7: "Ctrl+7",
        profile_8: "Ctrl+8",
        profile_9: "Ctrl+9",
    },
    HOTKEY_KEYS = Object.keys(DEFAULT_HOTKEYS) as (keyof HotkeySettings)[],
    RESERVED_HOTKEYS: Record<string, true> = {
        "Ctrl+I": true,
        "Ctrl+Minus": true,
        "Ctrl+Plus": true,
        "Ctrl+R": true,
        "Ctrl+Shift+Plus": true,
        "Ctrl+Shift+Space": true,
    };

export interface ConfigLoadResult {
    config: AppConfig;
    revision: number;
    error?: string;
}

export interface ConfigChange {
    config: AppConfig;
    revision: number;
    source: symbol | undefined;
}

export interface ConfigSaveOptions {
    expectedRevision?: number;
    source?: symbol;
}

export class ConfigConflictError extends Error {
    constructor(expectedRevision: number, currentRevision: number) {
        super(
            `Configuration changed since it was loaded (expected revision ${expectedRevision}, current revision ${currentRevision})`,
        );
        this.name = "ConfigConflictError";
    }
}

export function defaultConfig(): AppConfig {
    return {
        active_profile: "default",
        profiles: [{ id: "default", name: "Default", overlays: [] }],
        settings: {
            allow_unrestricted_shared_array_buffer: false,
            allowed_ips: DEFAULT_ALLOWED_IPS,
            always_keep_overlays_in_view: false,
            hotkeys: { ...DEFAULT_HOTKEYS },
            remote_access_host: "",
            remote_access_token: randomBytes(REMOTE_ACCESS_TOKEN_BYTES).toString("hex"),
        },
        version: CONFIG_VERSION,
    };
}

function finite(value: unknown, fallback: number): number {
    return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function normalizePlacement(value: unknown): Placement {
    const source =
            value && typeof value === "object" && !Array.isArray(value)
                ? (value as Record<string, unknown>)
                : {},
        placement: Placement = {
            counter_id: typeof source.counter_id === "string" ? source.counter_id : "",
            height: Math.max(10, finite(source.height, 300)),
            left: finite(source.left, 0),
            opacity: Math.min(1, Math.max(0, finite(source.opacity, 1))),
            rotation: Math.min(180, Math.max(-180, finite(source.rotation, 0))),
            scale: Math.max(0.1, finite(source.scale, 1)),
            top: finite(source.top, 0),
            width: Math.max(10, finite(source.width, 500)),
            z_index: Math.min(999, Math.max(0, Math.round(finite(source.z_index, 0)))),
        };
    if (typeof source.url === "string") {
        try {
            const url = new URL(source.url);
            if (["http:", "https:", "vosulay-overlay:"].includes(url.protocol)) {
                placement.url = url.toString();
            }
        } catch {
            // Invalid optional sources are discarded during normalization.
        }
    }
    if (typeof source.name === "string" && source.name.trim()) {
        placement.name = source.name.trim();
    }
    return placement;
}

function canonicalHotkey(value: unknown, fallback: string): string {
    if (typeof value !== "string") {
        return fallback;
    }
    const rawParts = value
        .split("+")
        .map((part) => part.trim())
        .filter(Boolean);
    if (rawParts.length === 0) {
        return fallback;
    }
    const aliases: Record<string, string> = {
            alt: "Alt",
            cmd: "Meta",
            command: "Meta",
            control: "Ctrl",
            ctrl: "Ctrl",
            meta: "Meta",
            option: "Alt",
            shift: "Shift",
            super: "Meta",
            win: "Meta",
            windows: "Meta",
        },
        keyNames: Record<string, string> = {
            arrowdown: "ArrowDown",
            arrowleft: "ArrowLeft",
            arrowright: "ArrowRight",
            arrowup: "ArrowUp",
            backspace: "Backspace",
            del: "Delete",
            delete: "Delete",
            end: "End",
            enter: "Enter",
            esc: "Escape",
            escape: "Escape",
            home: "Home",
            minus: "Minus",
            pagedown: "PageDown",
            pageup: "PageUp",
            plus: "Plus",
            return: "Enter",
            space: "Space",
            spacebar: "Space",
            tab: "Tab",
        },
        modifiers = new Set<string>();
    let key = "";
    for (const part of rawParts) {
        const modifier = aliases[part.toLowerCase()];
        if (modifier) {
            modifiers.add(modifier);
        } else if (key) {
            return fallback;
        } else {
            const lower = part.toLowerCase();
            key =
                keyNames[lower] ||
                (/^[a-z0-9]$/i.test(part)
                    ? part.toUpperCase()
                    : (/^f(?:[1-9]|1[0-2])$/i.test(part)
                      ? part.toUpperCase()
                      : ""));
        }
    }
    if (!key) {
        return fallback;
    }
    const shortcut = ["Ctrl", "Alt", "Shift", "Meta"]
        .filter((modifier) => modifiers.has(modifier))
        .concat(key)
        .join("+");
    return RESERVED_HOTKEYS[shortcut] ? fallback : shortcut;
}

function remoteAccessToken(value: unknown): string {
    return typeof value === "string" && /^[0-9a-fA-F]{64}$/.test(value.trim())
        ? value.trim().toLowerCase()
        : randomBytes(REMOTE_ACCESS_TOKEN_BYTES).toString("hex");
}

function remoteAccessHost(value: unknown): string {
    if (typeof value !== "string") {
        return "";
    }
    const candidate = value.trim().replace(/\.$/, "");
    if (!candidate) {
        return "";
    }
    try {
        const url = new URL(`http://${candidate}/`);
        if (
            url.username ||
            url.password ||
            url.port ||
            url.pathname !== "/" ||
            url.search ||
            url.hash
        ) {
            return "";
        }
        return url.hostname.replaceAll(/^\[|\]$/g, "").toLowerCase();
    } catch {
        return "";
    }
}

function normalizeSettings(value: unknown): AppSettings {
    const source = value && typeof value === "object" ? (value as Record<string, unknown>) : {},
        inputHotkeys =
            source.hotkeys && typeof source.hotkeys === "object"
                ? (source.hotkeys as Record<string, unknown>)
                : {},
        hotkeys = { ...DEFAULT_HOTKEYS };
    for (const key of HOTKEY_KEYS) {
        hotkeys[key] = canonicalHotkey(inputHotkeys[key], DEFAULT_HOTKEYS[key]);
    }
    return {
        allow_unrestricted_shared_array_buffer:
            source.allow_unrestricted_shared_array_buffer === true,
        allowed_ips:
            typeof source.allowed_ips === "string" ? source.allowed_ips : DEFAULT_ALLOWED_IPS,
        always_keep_overlays_in_view: source.always_keep_overlays_in_view === true,
        hotkeys,
        remote_access_host: remoteAccessHost(source.remote_access_host),
        remote_access_token: remoteAccessToken(source.remote_access_token),
    };
}

export function normalizeConfig(value: unknown): AppConfig {
    const source =
            value && typeof value === "object" && !Array.isArray(value)
                ? (value as Record<string, unknown>)
                : {},
        inputProfiles =
            Array.isArray(source.profiles) && source.profiles.length > 0
                ? source.profiles
                : [{ id: "default", name: "Default", overlays: [] }],
        usedIds = new Set<string>(),
        profiles: Profile[] = inputProfiles.map((entry, index) => {
            const profile =
                entry && typeof entry === "object" && !Array.isArray(entry)
                    ? (entry as Record<string, unknown>)
                    : {};
            let id = typeof profile.id === "string" ? profile.id : "";
            if (!id || usedIds.has(id)) {
                let suffix = index + 1;
                do {
                    id = `profile-${suffix}`;
                    suffix += 1;
                } while (usedIds.has(id));
            }
            usedIds.add(id);
            const name =
                    typeof profile.name === "string" && profile.name.trim()
                        ? profile.name
                        : `Profile ${index + 1}`,
                overlays = Array.isArray(profile.overlays)
                    ? profile.overlays.map(normalizePlacement)
                    : [];
            return { id, name, overlays };
        }),
        requested = typeof source.active_profile === "string" ? source.active_profile : "",
        firstProfile = profiles[0]!;
    return {
        active_profile: profiles.some((profile) => profile.id === requested)
            ? requested
            : firstProfile.id,
        profiles,
        settings: normalizeSettings(source.settings),
        version: CONFIG_VERSION,
    };
}

export function sharedArrayBufferBlinkSwitch(
    config: AppConfig,
): "enable-blink-features" | "disable-blink-features" {
    return config.settings.allow_unrestricted_shared_array_buffer
        ? "enable-blink-features"
        : "disable-blink-features";
}

export function loadConfigSync(configPath: string): AppConfig {
    try {
        const parsed = JSON.parse(readFileSync(configPath, "utf8")) as unknown;
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
            return defaultConfig();
        }
        if (!("version" in parsed) || parsed.version !== CONFIG_VERSION) {
            return defaultConfig();
        }
        return normalizeConfig(parsed);
    } catch {
        return defaultConfig();
    }
}

export class ConfigStore {
    readonly path: string;
    private saveTail: Promise<void> = Promise.resolve();
    private currentRevision = 0;
    private readonly subscribers = new Set<(change: ConfigChange) => void>();
    private saveError: Error | undefined = undefined;
    private fallbackConfig: AppConfig | undefined = undefined;

    constructor(configPath: string) {
        if (!configPath) {
            throw new Error("config path must not be empty");
        }
        this.path = configPath;
    }

    load(): Promise<ConfigLoadResult> {
        const operation = this.saveTail.then(async () => {
            try {
                const source = await readFile(this.path, "utf8"),
                    parsed = JSON.parse(source) as unknown;
                if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
                    throw new Error("configuration must be an object");
                }
                if ((parsed as Record<string, unknown>).version !== CONFIG_VERSION) {
                    throw new Error(
                        `unsupported config version ${String((parsed as Record<string, unknown>).version)}; expected ${CONFIG_VERSION}`,
                    );
                }
                const sourceToken = (
                        (parsed as Record<string, unknown>).settings as
                            | Record<string, unknown>
                            | undefined
                    )?.remote_access_token,
                    config = normalizeConfig(parsed);
                if (
                    typeof sourceToken !== "string" ||
                    !/^[0-9a-fA-F]{64}$/.test(sourceToken.trim())
                ) {
                    await this.writeAtomically(config);
                }
                return { config, revision: this.currentRevision };
            } catch (error) {
                if ((error as NodeJS.ErrnoException).code === "ENOENT") {
                    const config = defaultConfig();
                    await this.writeAtomically(config);
                    this.fallbackConfig = undefined;
                    return { config, revision: this.currentRevision };
                }
                const message = error instanceof Error ? error.message : String(error);
                this.fallbackConfig ??= defaultConfig();
                return {
                    config: this.fallbackConfig,
                    error: `Could not load configuration: ${message}`,
                    revision: this.currentRevision,
                };
            }
        });
        this.saveTail = operation.then(
            () => {},
            () => {},
        );
        return operation;
    }

    save(value: unknown, options: ConfigSaveOptions = {}): Promise<ConfigChange> {
        const config = normalizeConfig(value),
            operation = this.saveTail.then(async () => {
                if (
                    options.expectedRevision !== undefined &&
                    options.expectedRevision !== this.currentRevision
                ) {
                    throw new ConfigConflictError(options.expectedRevision, this.currentRevision);
                }
                await this.writeAtomically(config);
                this.fallbackConfig = undefined;
                const change = { config, revision: ++this.currentRevision, source: options.source };
                for (const subscriber of this.subscribers) {
                    try {
                        subscriber(change);
                    } catch {
                        // A consumer cannot roll back a configuration that was already committed.
                    }
                }
                return change;
            });
        this.saveTail = operation.then(
            () => {
                this.saveError = undefined;
            },
            (error: unknown) => {
                if (!(error instanceof ConfigConflictError)) {
                    this.saveError = error instanceof Error ? error : new Error(String(error));
                }
            },
        );
        return operation;
    }

    subscribe(subscriber: (change: ConfigChange) => void): () => void {
        this.subscribers.add(subscriber);
        return () => this.subscribers.delete(subscriber);
    }

    async flush(): Promise<void> {
        await this.saveTail;
        if (this.saveError !== undefined) {
            throw this.saveError;
        }
    }

    private async writeAtomically(config: AppConfig): Promise<void> {
        const directory = dirname(this.path);
        await mkdir(directory, { recursive: true });
        const temporary = join(
                directory,
                `.${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.tmp`,
            ),
            file = await open(
                temporary,
                constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
                0o600,
            );
        try {
            await file.writeFile(`${JSON.stringify(config, undefined, 2)}\n`, "utf8");
            await file.sync();
        } finally {
            await file.close();
        }
        try {
            await rename(temporary, this.path);
        } catch (error) {
            await rm(temporary, { force: true });
            throw error;
        }
    }
}
