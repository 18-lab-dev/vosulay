/*
 * Static string-keyed lookup tables stay object literals validated with
 * `satisfies`, so they keep their literal key types instead of widening into an
 * open dictionary that discards what the table actually contains. `tableValue`
 * is the single place where a dynamic key is applied to such a table.
 */
export function tableValue<Value>(table: Record<string, Value>, key: string): Value | undefined {
    return table[key];
}
