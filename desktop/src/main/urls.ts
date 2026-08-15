const SEGMENT_ESCAPES: Record<string, string> = {
    "%24": "$",
    "%26": "&",
    "%2B": "+",
    "%2C": ",",
    "%3A": ":",
    "%3B": ";",
    "%3D": "=",
    "%40": "@",
};

// Mirrors the Rust url crate's path-segment encode set used by
// Src/api.rs counter_url: encodeURIComponent over-escapes characters the
// Rust side leaves bare, so revert those to keep persisted counter URLs
// Byte-identical across platforms.
function encodePathSegment(segment: string): string {
    return encodeURIComponent(segment).replaceAll(
        /%2[46BC]|%3[ABD]|%40/gi,
        (escape) => SEGMENT_ESCAPES[escape.toUpperCase()] ?? escape,
    );
}

// Base URLs are normalized like the Rust side: query, fragment, and path are
// Replaced rather than rejected. Strict base validation belongs to option
// Parsing (see options.ts validateBaseUrl).
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
