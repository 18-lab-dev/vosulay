import { validateBaseUrl } from "./options";

function cleanBase(base: URL | string): URL {
    return validateBaseUrl(typeof base === "string" ? base : base.toString());
}

export function counterUrl(base: URL | string, counterId: string): URL {
    const url = cleanBase(base);
    url.search = "";
    url.hash = "";
    url.pathname = "/";
    url.pathname = `/${encodeURIComponent(counterId)}/`;
    return url;
}

export function settingsUrl(base: URL | string, counterId: string): URL {
    const url = new URL("/settings", cleanBase(base));
    url.searchParams.set("overlay", counterId);
    return url;
}

export function availableUrl(base: URL | string): URL {
    return new URL("/available", cleanBase(base));
}

export function counterSearchUrl(base: URL | string): URL {
    return new URL("/api/counters/search/", cleanBase(base));
}
