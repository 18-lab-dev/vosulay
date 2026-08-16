import { defineConfig } from "oxlint";

/*
 * Strict as hell: every stable oxlint category is denied, and the vendored
 * anti-slop plugin is fully enabled. The plugin lives in tools/oxlint/anti-slop
 * and is intentionally ignored below: it is vendored third-party code, not
 * vosulay source, and its own style is not ours to police.
 *
 * Everything under `rules` either relaxes a default that contradicts another
 * denied rule, is unreachable in source, or is a numeric shape metric whose
 * oxlint default was written for small library modules rather than an Electron
 * main process. No diagnostic is silenced because it was inconvenient to fix.
 */
export default defineConfig({
    categories: {
        correctness: "deny",
        pedantic: "deny",
        perf: "deny",
        style: "deny",
        suspicious: "deny",
    },
    ignorePatterns: ["out/**", "dist/**", "build/**", "tools/oxlint/anti-slop/**"],
    jsPlugins: [{ name: "anti-slop", specifier: "./tools/oxlint/anti-slop/index.ts" }],
    overrides: [
        {
            files: ["test/**"],
            rules: {
                /*
                 * An expected value written as a literal is the specification of
                 * the test. Importing the constant the implementation uses would
                 * make the assertion tautological, so tests keep their literals.
                 */
                "no-magic-numbers": "off",
            },
        },
        {
            files: ["src/main/types.ts"],
            rules: {
                /*
                 * TypeScript gives an implicit index signature to a type alias but
                 * never to an interface, so a configuration shape declared as an
                 * interface cannot travel inside `MessageEnvelope`. The data
                 * shapes in this module are therefore aliases by necessity, while
                 * behavioural contracts around them stay interfaces.
                 */
                "consistent-type-definitions": "off",
            },
        },
    ],
    rules: {
        "anti-slop/no-chained-type-assertions": "error",
        "anti-slop/no-conditional-empty-object-spread": "error",
        "anti-slop/no-known-value-widening": "error",
        "anti-slop/no-module-mocking": "error",
        "anti-slop/no-object-parameters": "error",
        "anti-slop/no-reflect-apply": "error",
        "anti-slop/no-reflect-get": "error",
        /*
         * Runtime `typeof` is only defensible inside a named type guard, which is
         * exactly the escape hatch this rule ships. TypeScript has no reflection
         * and this repository deliberately carries no schema dependency, so
         * `value is Foo` predicates are where external data becomes typed.
         */
        "anti-slop/no-runtime-typeof": ["error", { allowInTypeGuards: true }],
        "anti-slop/no-shape-in-symbol-names": "error",
        "anti-slop/no-unknown-parameters": "error",
        "anti-slop/no-unknown-returns": "error",
        "anti-slop/no-unknown-type-aliases": "error",
        "anti-slop/no-unsafe-dictionary-type": "error",
        "anti-slop/no-widen-then-assert": "error",
        "anti-slop/require-safety-comment-for-type-assertion": "error",
        /*
         * Oxlint 1.78 does not implement ESLint's `ignoreConsecutiveComments`, so
         * a wrapped `//` block would need a capital on every continuation line.
         * Multi-line prose is written as one block comment instead, which the
         * rule treats as a single sentence.
         */
        "capitalized-comments": "error",
        /*
         * `anti-slop/no-unknown-parameters` allows an `unknown` parameter only
         * when it is named `cause`, which is exactly what a promise rejection
         * handler receives. The two rules would otherwise deadlock over
         * `.catch((cause: unknown) => …)`, so `cause` is an accepted name here.
         */
        "catch-error-name": ["error", { ignore: ["cause"] }],
        /*
         * Oxlint defaults `func-style` to "expression". Every module-level helper
         * here is a hoisted declaration and every callback is an arrow; keep the
         * consistency check, invert the preference.
         */
        "func-style": ["error", "declaration", { allowArrowFunctions: true }],
        /*
         * The only sub-two-character names here are `x`/`y` screen coordinates,
         * which are clearer than any longer spelling.
         */
        "id-length": ["error", { exceptions: ["x", "y"], min: 2 }],
        /*
         * Code shape is reviewed, not gated numerically. Oxlint's defaults (10
         * statements, 50 lines, 3 parameters, 1 class per file) describe small
         * library modules; this process wires IPC, GPU transport, window
         * lifecycle, and an HTTP server, where the honest decomposition boundary
         * is semantic. A threshold picked to clear today's worst offender would
         * assert a limit nobody intends to hold, so these stay off.
         */
        "max-classes-per-file": "off",
        "max-lines": "off",
        "max-lines-per-function": "off",
        "max-nested-calls": "off",
        "max-params": "off",
        "max-statements": "off",
        /*
         * A `continue` guard keeps a loop body flat instead of nesting the real
         * work one level deeper. ESLint ships this rule off by default.
         */
        "no-continue": "off",
        /*
         * -1, 0, and 1 are ubiquitous (sentinels, indices, flags, offsets) and
         * ESLint's own default ignores them; flagging them would force hundreds
         * of noise constants. Genuine magic numbers are still flagged.
         */
        "no-magic-numbers": ["error", { ignore: [-1, 0, 1] }],
        /*
         * `no-ternary` and `prefer-ternary` are both inside the denied style set
         * and contradict each other. Conditional expressions are idiomatic here,
         * so `prefer-ternary` wins and the blanket ban is off.
         */
        "no-ternary": "off",
        /*
         * Oxfmt owns numeric literal casing and lowercases every hex digit, so
         * this rule's demand for uppercase digits is unsatisfiable while the
         * formatter runs. Separator grouping (`numeric-separators-style`) stays
         * on because oxfmt preserves it.
         */
        "number-literal-case": "off",
        /*
         * `one-var: "always"` (oxlint's default) demands a single `const` per
         * scope, which is unreachable in any function where a later binding
         * depends on an intervening statement or an early return. "never" is both
         * satisfiable and the idiom TypeScript tooling assumes.
         */
        "one-var": ["error", "never"],
        /*
         * Electron and Node surfaces this host talks to (ipcMain, child
         * processes, streams) are EventEmitter-based; EventTarget cannot
         * interoperate with them.
         */
        "prefer-event-target": "off",
        /*
         * Electron-vite bundles the main process as CommonJS, and rollup rejects
         * top-level await in that format, so the entrypoint must keep chaining
         * `start()` off a promise instead of awaiting it at module scope.
         */
        "prefer-top-level-await": "off",
    },
});
