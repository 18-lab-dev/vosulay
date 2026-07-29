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

function numericAttribute(element: AttributeElement, name: string): number | undefined {
    const source = element.attr(name);
    if (source === undefined) return undefined;
    const value = source.endsWith("px") ? source.slice(0, -2) : source;
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value)) return undefined;
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
}

export function parseInstalledCounters(base: URL | string, source: string): InstalledCounter[] {
    const $ = load(source);
    const cards = $("article.counter-card[data-overlay-id]");
    if (!cards.length && !$(".empty-state").length) {
        throw new Error("counter search markup was not recognized; vosu and vosulay versions may be out of sync");
    }

    const counters: InstalledCounter[] = [];
    cards.each((_index, node) => {
        const card = $(node);
        const id = requiredAttribute(card, "data-overlay-id", "counter card");
        if (!id) throw new Error("counter card has an empty data-overlay-id");
        const name = requiredAttribute(card, "data-counter-name", id);
        const author = requiredAttribute(card, "data-counter-author", id);
        const version = requiredAttribute(card, "data-counter-version", id);
        const preview = card.find(".counter-card__preview iframe").first();
        const width = numericAttribute(preview, "width");
        const height = numericAttribute(preview, "height");
        const validResolution = width !== undefined && height !== undefined && width > 10 && height > 10;
        counters.push({
            id,
            name,
            author,
            version,
            width: validResolution ? width : 500,
            height: validResolution ? height : 300,
            has_settings: card.find('[data-action="counter-settings"]').length > 0,
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
