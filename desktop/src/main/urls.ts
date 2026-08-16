import { tableValue } from "./tables";

const SEGMENT_ESCAPES = {
    "%24": "$",
    "%26": "&",
    "%2B": "+",
    "%2C": ",",
    "%3A": ":",
    "%3B": ";",
    "%3D": "=",
    "%40": "@",
} satisfies Record<string, string>;

const SEGMENT_ESCAPE_PATTERN = /%2[46BC]|%3[ABD]|%40/giu;

/*
 * Mirrors the Rust url crate's path-segment encode set used by src/api.rs
 * counter_url: encodeURIComponent over-escapes characters the Rust side leaves
 * bare, so revert those to keep persisted counter URLs byte-identical across
 * platforms.
 */
function encodePathSegment(segment: string): string {
    return encodeURIComponent(segment).replaceAll(
        SEGMENT_ESCAPE_PATTERN,
        (escape) => tableValue(SEGMENT_ESCAPES, escape.toUpperCase()) ?? escape,
    );
}

/*
 * Base URLs are normalized like the Rust side: query, fragment, and path are
 * replaced rather than rejected. Strict base validation belongs to option
 * parsing (see options.ts validateBaseUrl).
 */
export function counterUrl(base: URL | string, counterId: string): URL {
    const url = new URL(base);
    url.search = "";
    url.hash = "";
    url.pathname = `/${encodePathSegment(counterId)}/`;
    return url;
}

export function settingsUrl(base: URL | string, counterId: string): URL {
    const url = new URL("/settings", base);
    url.searchParams.set("overlay", counterId);
    return url;
}

export function availableUrl(base: URL | string): URL {
    return new URL("/available", base);
}

export function counterSearchUrl(base: URL | string): URL {
    return new URL("/api/counters/search/", base);
}
