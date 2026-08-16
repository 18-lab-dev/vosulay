import { loadConfigSync, normalizeConfig, sharedArrayBufferBlinkSwitch } from "../src/main/config";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import assert from "node:assert/strict";
import { join } from "node:path";
import { parseJsonText } from "../src/main/json";
import test from "node:test";
import { tmpdir } from "node:os";

const inputFixture = new URL("fixtures/config-parity-input.json", import.meta.url);
const expectedFixture = new URL("fixtures/config-parity-expected.json", import.meta.url);

test("TypeScript config normalization matches the canonical golden result", async () => {
    const input = parseJsonText(await readFile(inputFixture, "utf8"));
    const expected = parseJsonText(await readFile(expectedFixture, "utf8"));
    assert.ok(input !== undefined);
    assert.ok(expected !== undefined);

    // Hotkeys in the golden fixture use Electron's canonical representation.
    assert.deepEqual(normalizeConfig(input), expected);
});

test("SharedArrayBuffer startup switch is explicitly enabled or disabled", () => {
    const disabled = normalizeConfig({});
    const enabled = normalizeConfig({
        settings: { allow_unrestricted_shared_array_buffer: true },
    });

    assert.equal(sharedArrayBufferBlinkSwitch(disabled), "disable-blink-features");
    assert.equal(sharedArrayBufferBlinkSwitch(enabled), "enable-blink-features");
});

test("synchronous startup config loading fails closed", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-startup-config-"));
    const path = join(directory, "config.json");

    assert.equal(loadConfigSync(path).settings.allow_unrestricted_shared_array_buffer, false);
    await writeFile(path, "{bad", "utf8");
    assert.equal(loadConfigSync(path).settings.allow_unrestricted_shared_array_buffer, false);
    await writeFile(
        path,
        JSON.stringify({
            settings: { allow_unrestricted_shared_array_buffer: "true" },
            version: 1,
        }),
        "utf8",
    );
    assert.equal(loadConfigSync(path).settings.allow_unrestricted_shared_array_buffer, false);
    await writeFile(
        path,
        JSON.stringify(
            normalizeConfig({
                settings: { allow_unrestricted_shared_array_buffer: true },
            }),
        ),
        "utf8",
    );
    assert.equal(loadConfigSync(path).settings.allow_unrestricted_shared_array_buffer, true);
});
