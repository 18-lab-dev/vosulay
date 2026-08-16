import { counterSearchUrl, counterUrl } from "./urls";
import { type InstalledCounter } from "./types";
import { load } from "cheerio";
const DEFAULT_COUNTER_HEIGHT = 300;
const DEFAULT_COUNTER_WIDTH = 500;
const MIN_COUNTER_DIMENSION = 10;
const NUMBER_ATTRIBUTE_PATTERN = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/u;
const PIXEL_ATTRIBUTE_SUFFIX = "px";
const PIXEL_ATTRIBUTE_SUFFIX_LENGTH = PIXEL_ATTRIBUTE_SUFFIX.length;

export const MAX_COUNTER_PAGE_BYTES = 2_097_152;
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
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let source = "";
    let total = 0;
    for await (const value of response.body) {
        total += value.byteLength;
        if (total > limit) {
            throw new Error(`counter search response exceeds the ${limit} byte limit`);
        }
        source += decoder.decode(value, { stream: true });
    }
    source += decoder.decode();
    return source;
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
    const value = source.endsWith(PIXEL_ATTRIBUTE_SUFFIX)
        ? source.slice(0, -PIXEL_ATTRIBUTE_SUFFIX_LENGTH)
        : source;
    if (!NUMBER_ATTRIBUTE_PATTERN.test(value)) {
        return undefined;
    }
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
}
function normalizedCounterDimensions(width: number | undefined, height: number | undefined) {
    const validResolution =
        width !== undefined &&
        height !== undefined &&
        width > MIN_COUNTER_DIMENSION &&
        height > MIN_COUNTER_DIMENSION;
    if (validResolution) {
        return {
            height,
            width,
        };
    }
    return {
        height: DEFAULT_COUNTER_HEIGHT,
        width: DEFAULT_COUNTER_WIDTH,
    };
}

export function parseInstalledCounters(base: URL | string, source: string): InstalledCounter[] {
    const document = load(source);
    const results = document("article.counter-card[data-overlay-id], .result-item");
    const empty = document(".empty-state, .no-results");
    let recognized = false;
    const counters: InstalledCounter[] = [];
    results.each((_index, node) => {
        const result = document(node);
        const modernId = result.attr("data-overlay-id");
        if (modernId !== undefined) {
            recognized = true;
            const id = requiredAttribute(result, "data-overlay-id", "counter card");
            if (!id) {
                throw new Error("counter card has an empty data-overlay-id");
            }
            const name = requiredAttribute(result, "data-counter-name", id);
            const author = requiredAttribute(result, "data-counter-author", id);
            const version = requiredAttribute(result, "data-counter-version", id);
            const preview = result.find(".counter-card__preview iframe").first();
            const { width, height } = normalizedCounterDimensions(
                numericAttribute(preview, "width"),
                numericAttribute(preview, "height"),
            );
            counters.push({
                author,
                has_settings: result.find('[data-action="counter-settings"]').length > 0,
                height,
                id,
                name,
                url: counterUrl(base, id).toString(),
                version,
                width,
            });
            return;
        }

        const actions = result.find(".calu").first();
        if (actions.length === 0) {
            return;
        }
        recognized = true;
        const name = requiredAttribute(actions, "n", "legacy counter");
        const author = requiredAttribute(actions, "a", name);
        const version = requiredAttribute(actions, "v", name);
        const idElement = result
            .find(
                ".settings-builder-button[n], .settings-button[n], .open-button[n], .delete-button[n]",
            )
            .first();
        const id = idElement.attr("n");
        if (!id) {
            throw new Error(`${JSON.stringify(name)} is missing a legacy folder name`);
        }
        const preview = result.find(".ri-gallery iframe").first();
        const { width, height } = normalizedCounterDimensions(
            numericAttribute(preview, "width"),
            numericAttribute(preview, "height"),
        );
        counters.push({
            author,
            has_settings: actions.find(".settings-button").length > 0,
            height,
            id,
            name,
            url: counterUrl(base, id).toString(),
            version,
            width,
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
    const endpoint = counterSearchUrl(base);
    const controller = new AbortController();
    const timeout = setTimeout(
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
