import { constants, readFileSync } from "node:fs";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { AppConfig, AppSettings, HotkeySettings, Placement, Profile } from "./types";

const CONFIG_VERSION = 1 as const;
const DEFAULT_ALLOWED_IPS = "127.0.0.1,localhost,absolute";
const DEFAULT_HOTKEYS: HotkeySettings = {
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
    copy_overlay: "Ctrl+C",
    paste_overlay: "Ctrl+V",
};
const HOTKEY_KEYS = Object.keys(DEFAULT_HOTKEYS) as Array<keyof HotkeySettings>;
const RESERVED_HOTKEYS: Record<string, true> = {
    "Ctrl+I": true,
    "Ctrl+R": true,
    "Ctrl+Shift+Space": true,
    "Ctrl+Plus": true,
    "Ctrl+Shift+Plus": true,
    "Ctrl+Minus": true,
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
        super(`Configuration changed since it was loaded (expected revision ${expectedRevision}, current revision ${currentRevision})`);
        this.name = "ConfigConflictError";
    }
}


export function defaultConfig(): AppConfig {
    return {
        version: CONFIG_VERSION,
        active_profile: "default",
        profiles: [{ id: "default", name: "Default", overlays: [] }],
        settings: {
            allowed_ips: DEFAULT_ALLOWED_IPS,
            always_keep_overlays_in_view: false,
            allow_unrestricted_shared_array_buffer: false,
            hotkeys: { ...DEFAULT_HOTKEYS },
        },
    };
}

function finite(value: unknown, fallback: number): number {
    return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function normalizePlacement(value: unknown): Placement {
    const source = value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
    const placement: Placement = {
        counter_id: typeof source.counter_id === "string" ? source.counter_id : "",
        top: finite(source.top, 0),
        left: finite(source.left, 0),
        width: Math.max(10, finite(source.width, 500)),
        height: Math.max(10, finite(source.height, 300)),
        scale: Math.max(0.1, finite(source.scale, 1)),
        opacity: Math.min(1, Math.max(0, finite(source.opacity, 1))),
        rotation: Math.min(180, Math.max(-180, finite(source.rotation, 0))),
        z_index: Math.min(999, Math.max(0, Math.round(finite(source.z_index, 0)))),
    };
    if (typeof source.url === "string") {
        try {
            const url = new URL(source.url);
            if (["http:", "https:", "vosulay-overlay:"].includes(url.protocol)) placement.url = url.toString();
        } catch {
            // Invalid optional sources are discarded during normalization.
        }
    }
    if (typeof source.name === "string" && source.name.trim()) placement.name = source.name.trim();
    return placement;
}

function canonicalHotkey(value: unknown, fallback: string): string {
    if (typeof value !== "string") return fallback;
    const rawParts = value.split("+").map((part) => part.trim()).filter(Boolean);
    if (!rawParts.length) return fallback;
    const aliases: Record<string, string> = {
        control: "Ctrl", ctrl: "Ctrl", alt: "Alt", option: "Alt",
        shift: "Shift", meta: "Meta", command: "Meta", cmd: "Meta",
        super: "Meta", win: "Meta", windows: "Meta",
    };
    const keyNames: Record<string, string> = {
        esc: "Escape", escape: "Escape", space: "Space", spacebar: "Space",
        return: "Enter", enter: "Enter", del: "Delete", delete: "Delete",
        backspace: "Backspace", tab: "Tab", home: "Home", end: "End",
        pageup: "PageUp", pagedown: "PageDown", arrowup: "ArrowUp",
        arrowdown: "ArrowDown", arrowleft: "ArrowLeft", arrowright: "ArrowRight",
        plus: "Plus", minus: "Minus",
    };
    const modifiers = new Set<string>();
    let key = "";
    for (const part of rawParts) {
        const modifier = aliases[part.toLowerCase()];
        if (modifier) {
            modifiers.add(modifier);
        } else if (!key) {
            const lower = part.toLowerCase();
            key = keyNames[lower] || (/^[a-z0-9]$/i.test(part) ? part.toUpperCase()
                : (/^f(?:[1-9]|1[0-2])$/i.test(part) ? part.toUpperCase() : ""));
        } else {
            return fallback;
        }
    }
    if (!key) return fallback;
    const shortcut = ["Ctrl", "Alt", "Shift", "Meta"].filter((modifier) => modifiers.has(modifier)).concat(key).join("+");
    return RESERVED_HOTKEYS[shortcut] ? fallback : shortcut;
}

function normalizeSettings(value: unknown): AppSettings {
    const source = value && typeof value === "object" ? (value as Record<string, unknown>) : {};
    const inputHotkeys = source.hotkeys && typeof source.hotkeys === "object"
        ? (source.hotkeys as Record<string, unknown>)
        : {};
    const hotkeys = { ...DEFAULT_HOTKEYS };
    for (const key of HOTKEY_KEYS) hotkeys[key] = canonicalHotkey(inputHotkeys[key], DEFAULT_HOTKEYS[key]);
    return {
        allowed_ips: typeof source.allowed_ips === "string" ? source.allowed_ips : DEFAULT_ALLOWED_IPS,
        always_keep_overlays_in_view: source.always_keep_overlays_in_view === true,
        allow_unrestricted_shared_array_buffer: source.allow_unrestricted_shared_array_buffer === true,
        hotkeys,
    };
}

export function normalizeConfig(value: unknown): AppConfig {
    const source = value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
    const inputProfiles = Array.isArray(source.profiles) && source.profiles.length ? source.profiles : [{ id: "default", name: "Default", overlays: [] }];
    const usedIds = new Set<string>();
    const profiles: Profile[] = inputProfiles.map((entry, index) => {
        const profile = entry && typeof entry === "object" && !Array.isArray(entry) ? (entry as Record<string, unknown>) : {};
        let id = typeof profile.id === "string" ? profile.id : "";
        if (!id || usedIds.has(id)) {
            let suffix = index + 1;
            do {
                id = `profile-${suffix}`;
                suffix += 1;
            } while (usedIds.has(id));
        }
        usedIds.add(id);
        const name = typeof profile.name === "string" && profile.name.trim() ? profile.name : `Profile ${index + 1}`;
        const overlays = Array.isArray(profile.overlays) ? profile.overlays.map(normalizePlacement) : [];
        return { id, name, overlays };
    });
    const requested = typeof source.active_profile === "string" ? source.active_profile : "";
    const firstProfile = profiles[0]!;
    return {
        version: CONFIG_VERSION,
        active_profile: profiles.some((profile) => profile.id === requested) ? requested : firstProfile.id,
        profiles,
        settings: normalizeSettings(source.settings),
    };
}


export function sharedArrayBufferBlinkSwitch(config: AppConfig): "enable-blink-features" | "disable-blink-features" {
    return config.settings.allow_unrestricted_shared_array_buffer
        ? "enable-blink-features"
        : "disable-blink-features";
}

export function loadConfigSync(configPath: string): AppConfig {
    try {
        const parsed = JSON.parse(readFileSync(configPath, "utf8")) as unknown;
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return defaultConfig();
        if (!("version" in parsed) || parsed.version !== CONFIG_VERSION) return defaultConfig();
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
    private saveError: Error | null = null;

    constructor(configPath: string) {
        if (!configPath) throw new Error("config path must not be empty");
        this.path = configPath;
    }

    load(): Promise<ConfigLoadResult> {
        const operation = this.saveTail.then(async () => {
            try {
                const source = await readFile(this.path, "utf8");
                const parsed = JSON.parse(source) as unknown;
                if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("configuration must be an object");
                if ((parsed as Record<string, unknown>).version !== CONFIG_VERSION) {
                    throw new Error(`unsupported config version ${String((parsed as Record<string, unknown>).version)}; expected ${CONFIG_VERSION}`);
                }
                return { config: normalizeConfig(parsed), revision: this.currentRevision };
            } catch (error) {
                if ((error as NodeJS.ErrnoException).code === "ENOENT") {
                    return { config: defaultConfig(), revision: this.currentRevision };
                }
                const message = error instanceof Error ? error.message : String(error);
                return { config: defaultConfig(), revision: this.currentRevision, error: `Could not load configuration: ${message}` };
            }
        });
        this.saveTail = operation.then(
            () => undefined,
            () => undefined,
        );
        return operation;
    }

    save(value: unknown, options: ConfigSaveOptions = {}): Promise<ConfigChange> {
        const config = normalizeConfig(value);
        const operation = this.saveTail.then(async () => {
            if (options.expectedRevision !== undefined && options.expectedRevision !== this.currentRevision) {
                throw new ConfigConflictError(options.expectedRevision, this.currentRevision);
            }
            await this.writeAtomically(config);
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
                this.saveError = null;
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
        if (this.saveError !== null) throw this.saveError;
    }

    private async writeAtomically(config: AppConfig): Promise<void> {
        const directory = dirname(this.path);
        await mkdir(directory, { recursive: true });
        const temporary = join(directory, `.${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.tmp`);
        const file = await open(temporary, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY, 0o600);
        try {
            await file.writeFile(`${JSON.stringify(config, null, 2)}\n`, "utf8");
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
