export interface Placement {
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
}

export interface Profile {
    id: string;
    name: string;
    overlays: Placement[];
}

export interface AppConfig {
    version: 1;
    active_profile: string;
    profiles: Profile[];
}

export interface InstalledCounter {
    id: string;
    name: string;
    author: string;
    version: string;
    width: number;
    height: number;
    has_settings: boolean;
    url: string;
}

export interface EditingTarget {
    readonly editing: boolean;
    setEditing(editing: boolean): Promise<void>;
    toggleEditing(): Promise<void>;
}

export interface MessageEnvelope {
    type: string;
    [key: string]: unknown;
}

export interface WebContentsLike {
    readonly id?: number;
    send(channel: string, envelope: MessageEnvelope): void;
    isDestroyed?(): boolean;
}
