import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { LocalOverlayRegistry, loadLocalOverlayCapabilityKey } from "../src/main/local-overlays";

test("local overlay capabilities are installation-bound and persist outside profiles", async () => {
    const dataDirectory = await mkdtemp(join(tmpdir(), "vosulay-capability-")),
        first = loadLocalOverlayCapabilityKey(dataDirectory),
        second = loadLocalOverlayCapabilityKey(dataDirectory);
    assert.equal(first.length, 32);
    assert.deepEqual(second, first);
    assert.deepEqual(await readFile(join(dataDirectory, "local-overlay-capability.key")), first);

    const overlayRoot = join(dataDirectory, "overlay");
    await mkdir(overlayRoot);
    const entry = join(overlayRoot, "index.html");
    await writeFile(entry, "ok", "utf8");

    const issuer = new LocalOverlayRegistry(first),
        url = await issuer.createUrl(entry);
    assert.equal(await issuer.resolveAuthorizedFile(url), entry);

    const sameInstallation = new LocalOverlayRegistry(second);
    assert.equal(sameInstallation.authorizeUrl(url), true);
    assert.equal(await sameInstallation.resolveAuthorizedFile(url), entry);

    const otherInstallation = new LocalOverlayRegistry(Buffer.alloc(32, 0xA5));
    assert.equal(otherInstallation.authorizeUrl(url), false);
    await assert.rejects(otherInstallation.resolveAuthorizedFile(url));
});

test("an imported profile cannot mint an arbitrary readable-root capability", () => {
    const registry = new LocalOverlayRegistry(Buffer.alloc(32, 0x11)),
        forgedRoot = Buffer.from("/", "utf8").toString("base64url"),
        forged = `vosulay-overlay://${"a".repeat(32)}/${forgedRoot}/etc/passwd`;

    assert.equal(registry.authorizeUrl(forged), false);
    assert.equal(registry.authorizeUrls([forged]), false);
});
