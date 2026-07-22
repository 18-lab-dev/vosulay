import { constants } from "node:fs";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { AppConfig, Placement, Profile } from "./types";

const CONFIG_VERSION = 1 as const;

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
    };
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
