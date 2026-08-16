import type { AppConfig, AppSettings, HotkeySettings, Placement, Profile } from "./types";
import {
    type JsonObject,
    type JsonValue,
    isJsonArray,
    isJsonNumber,
    isJsonObject,
    isJsonString,
    parseJsonText,
} from "./json";
import { constants, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { randomBytes } from "node:crypto";
import { tableValue } from "./tables";

const CONFIG_VERSION = 1 as const;
const DEFAULT_ALLOWED_IPS = "127.0.0.1,localhost,absolute";
const REMOTE_ACCESS_TOKEN_BYTES = 32;
const REMOTE_ACCESS_TOKEN_PATTERN = /^[0-9a-fA-F]{64}$/u;
const SINGLE_KEY_PATTERN = /^[a-z0-9]$/iu;
const FUNCTION_KEY_PATTERN = /^f(?:[1-9]|1[0-2])$/iu;
const TRAILING_DOT_PATTERN = /\.$/u;
const IPV6_BRACKET_PATTERN = /^\[|\]$/gu;
const MIN_PLACEMENT_SIZE = 10;
const DEFAULT_PLACEMENT_HEIGHT = 300;
const DEFAULT_PLACEMENT_WIDTH = 500;
const MIN_PLACEMENT_SCALE = 0.1;
const MAX_PLACEMENT_ROTATION = 180;
const MAX_PLACEMENT_Z_INDEX = 999;
const CONFIG_JSON_INDENT = 2;
const CONFIG_FILE_MODE = 0o600;
const TEMPORARY_SUFFIX_RADIX = 16;
const TEMPORARY_SUFFIX_START = 2;
const OVERLAY_URL_PROTOCOLS = new Set(["http:", "https:", "vosulay-overlay:"]);
const MODIFIER_ORDER = ["Ctrl", "Alt", "Shift", "Meta"];

const DEFAULT_HOTKEYS: HotkeySettings = {
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
};

/* SAFETY: The keys of a `HotkeySettings` literal are exactly its own property names. */
const HOTKEY_KEYS = Object.keys(DEFAULT_HOTKEYS) as (keyof HotkeySettings)[];

/* Shortcuts the manager reserves for itself; a profile may not rebind them. */
const RESERVED_HOTKEYS = {
    "Ctrl+I": true,
    "Ctrl+Minus": true,
    "Ctrl+Plus": true,
    "Ctrl+R": true,
    "Ctrl+Shift+Plus": true,
    "Ctrl+Shift+Space": true,
} satisfies Record<string, true>;

const MODIFIER_ALIASES = {
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
} satisfies Record<string, string>;

const KEY_NAMES = {
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
} satisfies Record<string, string>;

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

function isErrnoException(cause: unknown): cause is NodeJS.ErrnoException {
    return cause instanceof Error && "code" in cause;
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

function finite(value: JsonValue | undefined, fallback: number): number {
    return isJsonNumber(value) ? value : fallback;
}

function normalizePlacement(value: JsonValue | undefined): Placement {
    const source: JsonObject = isJsonObject(value) ? value : {};
    const placement: Placement = {
        counter_id: isJsonString(source.counter_id) ? source.counter_id : "",
        height: Math.max(MIN_PLACEMENT_SIZE, finite(source.height, DEFAULT_PLACEMENT_HEIGHT)),
        left: finite(source.left, 0),
        opacity: Math.min(1, Math.max(0, finite(source.opacity, 1))),
        rotation: Math.min(
            MAX_PLACEMENT_ROTATION,
            Math.max(-MAX_PLACEMENT_ROTATION, finite(source.rotation, 0)),
        ),
        scale: Math.max(MIN_PLACEMENT_SCALE, finite(source.scale, 1)),
        top: finite(source.top, 0),
        width: Math.max(MIN_PLACEMENT_SIZE, finite(source.width, DEFAULT_PLACEMENT_WIDTH)),
        z_index: Math.min(
            MAX_PLACEMENT_Z_INDEX,
            Math.max(0, Math.round(finite(source.z_index, 0))),
        ),
    };
    if (isJsonString(source.url)) {
        try {
            const url = new URL(source.url);
            if (OVERLAY_URL_PROTOCOLS.has(url.protocol)) {
                placement.url = url.toString();
            }
        } catch {
            /* Invalid optional sources are discarded during normalization. */
        }
    }
    if (isJsonString(source.name) && source.name.trim()) {
        placement.name = source.name.trim();
    }
    return placement;
}

function canonicalHotkey(value: JsonValue | undefined, fallback: string): string {
    if (!isJsonString(value)) {
        return fallback;
    }
    const rawParts = value
        .split("+")
        .map((part) => part.trim())
        .filter(Boolean);
    if (rawParts.length === 0) {
        return fallback;
    }
    const modifiers = new Set<string>();
    let key = "";
    for (const part of rawParts) {
        const lower = part.toLowerCase();
        const modifier = tableValue(MODIFIER_ALIASES, lower);
        if (modifier !== undefined) {
            modifiers.add(modifier);
        } else if (key) {
            return fallback;
        } else {
            const named = tableValue(KEY_NAMES, lower);
            const printable =
                SINGLE_KEY_PATTERN.test(part) || FUNCTION_KEY_PATTERN.test(part)
                    ? part.toUpperCase()
                    : "";
            key = named ?? printable;
        }
    }
    if (!key) {
        return fallback;
    }
    const shortcut = [...MODIFIER_ORDER.filter((modifier) => modifiers.has(modifier)), key].join(
        "+",
    );
    return tableValue(RESERVED_HOTKEYS, shortcut) === true ? fallback : shortcut;
}

function remoteAccessToken(value: JsonValue | undefined): string {
    return isJsonString(value) && REMOTE_ACCESS_TOKEN_PATTERN.test(value.trim())
        ? value.trim().toLowerCase()
        : randomBytes(REMOTE_ACCESS_TOKEN_BYTES).toString("hex");
}

function remoteAccessHost(value: JsonValue | undefined): string {
    if (!isJsonString(value)) {
        return "";
    }
    const candidate = value.trim().replace(TRAILING_DOT_PATTERN, "");
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
        return url.hostname.replaceAll(IPV6_BRACKET_PATTERN, "").toLowerCase();
    } catch {
        return "";
    }
}

function normalizeSettings(value: JsonValue | undefined): AppSettings {
    const source: JsonObject = isJsonObject(value) ? value : {};
    const inputHotkeys: JsonObject = isJsonObject(source.hotkeys) ? source.hotkeys : {};
    const hotkeys = { ...DEFAULT_HOTKEYS };
    for (const key of HOTKEY_KEYS) {
        hotkeys[key] = canonicalHotkey(inputHotkeys[key], DEFAULT_HOTKEYS[key]);
    }
    return {
        allow_unrestricted_shared_array_buffer:
            source.allow_unrestricted_shared_array_buffer === true,
        allowed_ips: isJsonString(source.allowed_ips) ? source.allowed_ips : DEFAULT_ALLOWED_IPS,
        always_keep_overlays_in_view: source.always_keep_overlays_in_view === true,
        hotkeys,
        remote_access_host: remoteAccessHost(source.remote_access_host),
        remote_access_token: remoteAccessToken(source.remote_access_token),
    };
}

export function normalizeConfig(value: JsonValue | undefined): AppConfig {
    const source: JsonObject = isJsonObject(value) ? value : {};
    const inputProfiles: readonly JsonValue[] =
        isJsonArray(source.profiles) && source.profiles.length > 0
            ? source.profiles
            : [{ id: "default", name: "Default", overlays: [] }];
    const usedIds = new Set<string>();
    const profiles: Profile[] = inputProfiles.map((entry, index) => {
        const profile: JsonObject = isJsonObject(entry) ? entry : {};
        let id = isJsonString(profile.id) ? profile.id : "";
        if (!id || usedIds.has(id)) {
            let suffix = index + 1;
            do {
                id = `profile-${suffix}`;
                suffix += 1;
            } while (usedIds.has(id));
        }
        usedIds.add(id);
        const name =
            isJsonString(profile.name) && profile.name.trim()
                ? profile.name
                : `Profile ${index + 1}`;
        const overlays = isJsonArray(profile.overlays)
            ? profile.overlays.map((overlay) => normalizePlacement(overlay))
            : [];
        return { id, name, overlays };
    });
    const requested = isJsonString(source.active_profile) ? source.active_profile : "";
    const firstProfile = profiles[0]!;
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
        const parsed = parseJsonText(readFileSync(configPath, "utf8"));
        if (!isJsonObject(parsed)) {
            return defaultConfig();
        }
        if (parsed.version !== CONFIG_VERSION) {
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
                const source = await readFile(this.path, "utf8");
                /*
                 * `JSON.parse` is typed `any` but only yields JSON's own value
                 * space, which is `JsonValue`. Malformed text throws here on
                 * purpose: the catch below turns it into a load error.
                 */
                const parsed: JsonValue = JSON.parse(source);
                if (!isJsonObject(parsed)) {
                    throw new Error("configuration must be an object");
                }
                if (parsed.version !== CONFIG_VERSION) {
                    throw new Error(
                        `unsupported config version ${String(parsed.version)}; expected ${CONFIG_VERSION}`,
                    );
                }
                const { settings } = parsed;
                const sourceToken = isJsonObject(settings)
                    ? settings.remote_access_token
                    : undefined;
                const config = normalizeConfig(parsed);
                if (
                    !isJsonString(sourceToken) ||
                    !REMOTE_ACCESS_TOKEN_PATTERN.test(sourceToken.trim())
                ) {
                    await this.writeAtomically(config);
                }
                return { config, revision: this.currentRevision };
            } catch (error) {
                if (isErrnoException(error) && error.code === "ENOENT") {
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

    save(value: JsonValue | undefined, options: ConfigSaveOptions = {}): Promise<ConfigChange> {
        const config = normalizeConfig(value);
        const operation = this.saveTail.then(async () => {
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
                    /* A consumer cannot roll back a configuration that was already committed. */
                }
            }
            return change;
        });
        this.saveTail = operation.then(
            () => {
                this.saveError = undefined;
            },
            (cause: unknown) => {
                if (!(cause instanceof ConfigConflictError)) {
                    this.saveError = cause instanceof Error ? cause : new Error(String(cause));
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
        const suffix = Math.random().toString(TEMPORARY_SUFFIX_RADIX).slice(TEMPORARY_SUFFIX_START);
        const temporary = join(directory, `.${process.pid}-${Date.now()}-${suffix}.tmp`);
        const file = await open(
            temporary,
            constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
            CONFIG_FILE_MODE,
        );
        try {
            await file.writeFile(
                `${JSON.stringify(config, undefined, CONFIG_JSON_INDENT)}\n`,
                "utf8",
            );
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
