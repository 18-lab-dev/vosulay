import { load } from "cheerio";
import type { InstalledCounter } from "./types";
import { counterSearchUrl, counterUrl } from "./urls";

export const MAX_COUNTER_PAGE_BYTES = 2 * 1024 * 1024;
export const COUNTER_FETCH_TIMEOUT_MS = 10_000;
interface AttributeElement {
    attr(name: string): string | undefined;
}

export async function readLimitedResponse(response: Response, limit = MAX_COUNTER_PAGE_BYTES): Promise<string> {
    const declared = response.headers.get("content-length");
    if (declared && Number(declared) > limit) throw new Error(`counter search response exceeds the ${limit} byte limit`);
    if (!response.body) return "";
    const reader = response.body.getReader();
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let total = 0;
    let source = "";
    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) break;
            total += value.byteLength;
            if (total > limit) throw new Error(`counter search response exceeds the ${limit} byte limit`);
            source += decoder.decode(value, { stream: true });
        }
        source += decoder.decode();
        return source;
    } finally {
        reader.releaseLock();
    }
}

function requiredAttribute(element: AttributeElement, name: string, counter: string): string {
    const value = element.attr(name);
    if (value === undefined) throw new Error(`${JSON.stringify(counter)} is missing the ${JSON.stringify(name)} attribute`);
    return value;
}

export function parseInstalledCounters(base: URL | string, source: string): InstalledCounter[] {
    const $ = load(source);
    const counters: InstalledCounter[] = [];
    $(".result-item[data-overlay-id]").each((_index, node) => {
        const card = $(node);
        const id = requiredAttribute(card, "data-overlay-id", "counter card");
        if (!id) throw new Error("counter card has an empty data-overlay-id");
        const metadata = card.find(".calu[n][a][v]").first();
        if (!metadata.length) throw new Error(`counter ${JSON.stringify(id)} is missing metadata`);
        const width = Number(card.find('[nft="width"][nfv]').first().attr("nfv"));
        const height = Number(card.find('[nft="height"][nfv]').first().attr("nfv"));
        const validResolution = Number.isFinite(width) && Number.isFinite(height) && width > 10 && height > 10;
        counters.push({
            id,
            name: requiredAttribute(metadata, "n", id),
            author: requiredAttribute(metadata, "a", id),
            version: requiredAttribute(metadata, "v", id),
            width: validResolution ? width : 500,
            height: validResolution ? height : 300,
            has_settings: card.find(".settings-button").length > 0,
            url: counterUrl(base, id).toString(),
        });
    });
    return counters;
}

export async function fetchInstalledCounters(
    base: URL | string,
    fetcher: typeof fetch = globalThis.fetch,
    timeoutMs = COUNTER_FETCH_TIMEOUT_MS,
): Promise<InstalledCounter[]> {
    const endpoint = counterSearchUrl(base);
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(new Error("counter search timed out")), timeoutMs);
    try {
        const response = await fetcher(endpoint, { signal: controller.signal });
        if (!response.ok) throw new Error(`counter search at ${endpoint} returned HTTP status ${response.status}`);
        const contentType = response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
        if (contentType !== "text/html" && contentType !== "application/xhtml+xml") {
            throw new Error(`counter search at ${endpoint} returned non-HTML content type ${contentType ?? "missing"}`);
        }
        return parseInstalledCounters(base, await readLimitedResponse(response));
    } finally {
        clearTimeout(timeout);
    }
}
