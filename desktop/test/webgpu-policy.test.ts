import assert from "node:assert/strict";
import test from "node:test";

import { buildManagerDocumentFromSources } from "../src/main/manager-document";
import { parseOptions } from "../src/main/options";
import { renderProfileDocument } from "../src/main/remote-server";
import type { Profile } from "../src/main/types";

test("the in-game manager delegates WebGPU only to trusted source classes", () => {
    const document = buildManagerDocumentFromSources(
        "<script>__VOSULAY_BOOTSTRAP__</script>",
        "",
        "",
        60,
        "http://127.0.0.1:24050",
    );
    assert.match(document, /vosulay-overlay:/);
    assert.match(document, /http:\/\/127\.0\.0\.1:24050/);
    assert.match(document, /HTMLIFrameElement\.prototype\.setAttribute/);
    assert.match(document, /part\.toLowerCase\(\)!=="webgpu"/);
});

test("remote profile pages do not grant WebGPU to arbitrary URLs", () => {
    const profile: Profile = {
            id: "one",
            name: "One",
            overlays: [
                {
                    counter_id: "counter",
                    height: 100,
                    left: 0,
                    opacity: 1,
                    rotation: 0,
                    scale: 1,
                    top: 0,
                    width: 100,
                    z_index: 0,
                },
                {
                    counter_id: "custom",
                    height: 100,
                    left: 0,
                    opacity: 1,
                    rotation: 0,
                    scale: 1,
                    top: 0,
                    url: "https://untrusted.example/overlay",
                    width: 100,
                    z_index: 1,
                },
                {
                    counter_id: "local",
                    height: 100,
                    left: 0,
                    opacity: 1,
                    rotation: 0,
                    scale: 1,
                    top: 0,
                    url: `vosulay-overlay://${"a".repeat(32)}/dG1w/index.html`,
                    width: 100,
                    z_index: 2,
                },
            ],
        },
        document = renderProfileDocument(
            profile,
            parseOptions(["--vosu-url", "http://127.0.0.1:24050"]),
            "0".repeat(64),
        );

    assert.match(document, /src="https:\/\/untrusted\.example\/overlay"[^>]*allow="autoplay"/);
    assert.doesNotMatch(
        document,
        /src="https:\/\/untrusted\.example\/overlay"[^>]*allow="[^"]*webgpu/,
    );
    assert.match(
        document,
        /src="http:\/\/127\.0\.0\.1:24050\/counter\/"[^>]*allow="autoplay; webgpu"/,
    );
    assert.match(
        document,
        /\/access\/[0-9a-f]+\/local\/[0-9a-f]+\/index\.html[^>]*allow="autoplay; webgpu"/,
    );
});
