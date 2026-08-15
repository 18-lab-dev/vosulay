import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { loadConfigSync, normalizeConfig, sharedArrayBufferBlinkSwitch } from "../src/main/config";

const inputFixture = new URL("fixtures/config-parity-input.json", import.meta.url),
    expectedFixture = new URL("fixtures/config-parity-expected.json", import.meta.url);

test("TypeScript config normalization matches the canonical golden result", async () => {
    const input = JSON.parse(await readFile(inputFixture, "utf8")) as unknown,
        expected = JSON.parse(await readFile(expectedFixture, "utf8")) as unknown;

    // Hotkeys in the golden fixture use Electron's canonical representation.
    assert.deepEqual(normalizeConfig(input), expected);
});

test("SharedArrayBuffer startup switch is explicitly enabled or disabled", () => {
    const disabled = normalizeConfig({}),
        enabled = normalizeConfig({
            settings: { allow_unrestricted_shared_array_buffer: true },
        });

    assert.equal(sharedArrayBufferBlinkSwitch(disabled), "disable-blink-features");
    assert.equal(sharedArrayBufferBlinkSwitch(enabled), "enable-blink-features");
});

test("synchronous startup config loading fails closed", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-startup-config-")),
        path = join(directory, "config.json");

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
