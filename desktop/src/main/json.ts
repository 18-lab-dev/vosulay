/*
 * Every value that reaches this process from outside it — the configuration
 * file, manager IPC traffic, the native addon's event strings — arrives as JSON
 * text. This module is the single boundary where such a payload becomes typed:
 * `parseJsonText` turns text into a `JsonValue`, and the guards below narrow one
 * `JsonValue` into the shape a caller needs. Runtime `typeof` belongs here, in a
 * named type predicate, and nowhere else.
 */

export type JsonValue = JsonArray | JsonObject | boolean | null | number | string;

export type JsonArray = readonly JsonValue[];

export interface JsonObject {
    readonly [key: string]: JsonValue | undefined;
}

export function isJsonObject(value: JsonValue | undefined): value is JsonObject {
    return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isJsonArray(value: JsonValue | undefined): value is JsonArray {
    return Array.isArray(value);
}

export function isJsonString(value: JsonValue | undefined): value is string {
    return typeof value === "string";
}

/** JSON cannot carry NaN or Infinity, so a numeric payload that is not finite is malformed. */
export function isJsonNumber(value: JsonValue | undefined): value is number {
    return typeof value === "number" && Number.isFinite(value);
}

export function isJsonBoolean(value: JsonValue | undefined): value is boolean {
    return typeof value === "boolean";
}

/** Decode JSON text, reporting malformed input as `undefined` rather than throwing. */
export function parseJsonText(text: string): JsonValue | undefined {
    try {
        /*
         * `JSON.parse` is typed `any` by the standard library, but it only ever
         * produces values inside JSON's own value space, which is exactly
         * `JsonValue`. Every narrowing past this point goes through a guard.
         */
        const decoded: JsonValue = JSON.parse(text);
        return decoded;
    } catch {
        return undefined;
    }
}
