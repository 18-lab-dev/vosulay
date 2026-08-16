import type { JsonValue } from "./json";

/*
 * Configuration shapes are declared as type aliases rather than interfaces so
 * they carry an implicit index signature and can travel inside a
 * `MessageEnvelope` without being re-copied into a parallel JSON type.
 */

export type Placement = {
    counter_id: string;
    url?: string;
    name?: string;
    top: number;
    left: number;
    width: number;
    height: number;
    scale: number;
    opacity: number;
    rotation: number;
    z_index: number;
};

export type Profile = {
    id: string;
    name: string;
    overlays: Placement[];
};

export type HotkeySettings = {
    profile_0: string;
    profile_1: string;
    profile_2: string;
    profile_3: string;
    profile_4: string;
    profile_5: string;
    profile_6: string;
    profile_7: string;
    profile_8: string;
    profile_9: string;
    copy_overlay: string;
    paste_overlay: string;
};

export type AppSettings = {
    allowed_ips: string;
    remote_access_token: string;
    remote_access_host: string;
    always_keep_overlays_in_view: boolean;
    allow_unrestricted_shared_array_buffer: boolean;
    hotkeys: HotkeySettings;
};

export type AppConfig = {
    version: 1;
    active_profile: string;
    profiles: Profile[];
    settings: AppSettings;
};

export type InstalledCounter = {
    id: string;
    name: string;
    author: string;
    version: string;
    width: number;
    height: number;
    has_settings: boolean;
    url: string;
};

export interface EditingTarget {
    readonly editing: boolean;
    setEditing: (editing: boolean) => Promise<void>;
    toggleEditing: () => Promise<void>;
}

/** A manager message: a discriminating `type` plus JSON-encodable payload fields. */
export interface MessageEnvelope {
    type: string;
    [key: string]: JsonValue | undefined;
}

export interface WebContentsLike {
    readonly id?: number;
    send: (channel: string, envelope: MessageEnvelope) => void;
    isDestroyed?: () => boolean;
}
