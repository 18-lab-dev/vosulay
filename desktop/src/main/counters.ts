import { load } from "cheerio";
import type { InstalledCounter } from "./types";
import { counterSearchUrl, counterUrl } from "./urls";

export const MAX_COUNTER_PAGE_BYTES = 2 * 1024 * 1024;
export const COUNTER_FETCH_TIMEOUT_MS = 10_000;
interface AttributeElement {
    attr: (name: string) => string | undefined;
}

export async function readLimitedResponse(
    response: Response,
    limit = MAX_COUNTER_PAGE_BYTES,
): Promise<string> {
    const declared = response.headers.get("content-length");
    if (declared && Number(declared) > limit) {
        throw new Error(`counter search response exceeds the ${limit} byte limit`);
    }
    if (!response.body) {
        return "";
    }
    const reader = response.body.getReader(),
        decoder = new TextDecoder("utf-8", { fatal: true });
    let source = "",
        total = 0;
    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) {
                break;
            }
            total += value.byteLength;
            if (total > limit) {
                throw new Error(`counter search response exceeds the ${limit} byte limit`);
            }
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
    if (value === undefined) {
        throw new Error(
            `${JSON.stringify(counter)} is missing the ${JSON.stringify(name)} attribute`,
        );
    }
    return value;
}

function numericAttribute(element: AttributeElement, name: string): number | undefined {
    const source = element.attr(name);
    if (source === undefined) {
        return undefined;
    }
    const value = source.endsWith("px") ? source.slice(0, -2) : source;
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value)) {
        return undefined;
    }
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
}

export function parseInstalledCounters(base: URL | string, source: string): InstalledCounter[] {
    const $ = load(source),
        results = $("article.counter-card[data-overlay-id], .result-item"),
        empty = $(".empty-state, .no-results");
    let recognized = false;
    const counters: InstalledCounter[] = [];
    results.each((_index, node) => {
        const result = $(node),
            modernId = result.attr("data-overlay-id");
        if (modernId !== undefined) {
            recognized = true;
            const id = requiredAttribute(result, "data-overlay-id", "counter card");
            if (!id) {
                throw new Error("counter card has an empty data-overlay-id");
            }
            const name = requiredAttribute(result, "data-counter-name", id),
                author = requiredAttribute(result, "data-counter-author", id),
                version = requiredAttribute(result, "data-counter-version", id),
                preview = result.find(".counter-card__preview iframe").first(),
                width = numericAttribute(preview, "width"),
                height = numericAttribute(preview, "height"),
                validResolution =
                    width !== undefined && height !== undefined && width > 10 && height > 10;
            counters.push({
                author,
                has_settings: result.find('[data-action="counter-settings"]').length > 0,
                height: validResolution ? height : 300,
                id,
                name,
                url: counterUrl(base, id).toString(),
                version,
                width: validResolution ? width : 500,
            });
            return;
        }

        const actions = result.find(".calu").first();
        if (actions.length === 0) {
            return;
        }
        recognized = true;
        const name = requiredAttribute(actions, "n", "legacy counter"),
            author = requiredAttribute(actions, "a", name),
            version = requiredAttribute(actions, "v", name),
            idElement = result
                .find(
                    ".settings-builder-button[n], .settings-button[n], .open-button[n], .delete-button[n]",
                )
                .first(),
            id = idElement.attr("n");
        if (!id) {
            throw new Error(`${JSON.stringify(name)} is missing a legacy folder name`);
        }
        const preview = result.find(".ri-gallery iframe").first(),
            width = numericAttribute(preview, "width"),
            height = numericAttribute(preview, "height"),
            validResolution =
                width !== undefined && height !== undefined && width > 10 && height > 10;
        counters.push({
            author,
            has_settings: actions.find(".settings-button").length > 0,
            height: validResolution ? height : 300,
            id,
            name,
            url: counterUrl(base, id).toString(),
            version,
            width: validResolution ? width : 500,
        });
    });
    if (!recognized && empty.length === 0) {
        throw new Error(
            "counter search markup was not recognized; expected modern Vosu or legacy Tosu results",
        );
    }
    return counters;
}

export async function fetchInstalledCounters(
    base: URL | string,
    fetcher: typeof fetch = globalThis.fetch,
    timeoutMs = COUNTER_FETCH_TIMEOUT_MS,
): Promise<InstalledCounter[]> {
    const endpoint = counterSearchUrl(base),
        controller = new AbortController(),
        timeout = setTimeout(
            () => controller.abort(new Error("counter search timed out")),
            timeoutMs,
        );
    try {
        const response = await fetcher(endpoint, { signal: controller.signal });
        if (!response.ok) {
            throw new Error(
                `counter search at ${endpoint} returned HTTP status ${response.status}`,
            );
        }
        const contentType = response.headers
            .get("content-type")
            ?.split(";", 1)[0]
            ?.trim()
            .toLowerCase();
        if (contentType !== "text/html" && contentType !== "application/xhtml+xml") {
            throw new Error(
                `counter search at ${endpoint} returned non-HTML content type ${contentType ?? "missing"}`,
            );
        }
        return parseInstalledCounters(base, await readLimitedResponse(response));
    } finally {
        clearTimeout(timeout);
    }
}
