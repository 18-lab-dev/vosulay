import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { normalizeConfig } from "../src/main/config";
import { LocalOverlayRegistry } from "../src/main/local-overlays";
import { isPeerAllowed, RemoteOverlayServer } from "../src/main/remote-server";
import type { AppConfig } from "../src/main/types";

const runtime = {
    vosuUrl: new URL("http://127.0.0.1:24050/base/"),
    fpsLimit: 60,
    pids: [],
    processNames: [],
};

test("allowed_ips defaults exactly and preserves the configured peer list", () => {
    assert.equal(normalizeConfig({}).settings.allowed_ips, "127.0.0.1,localhost,absolute");
    assert.equal(normalizeConfig({ settings: { allowed_ips: "10.0.*.* ::1" } }).settings.allowed_ips, "10.0.*.* ::1");
    assert.equal(normalizeConfig({ settings: { allowed_ips: 42 } }).settings.allowed_ips, "127.0.0.1,localhost,absolute");
    assert.equal(normalizeConfig({ settings: { allowed_ips: "" } }).settings.allowed_ips, "");
});

test("peer authorization accepts loopback, exact IPs, and four-octet wildcard patterns", () => {
    assert.equal(isPeerAllowed("127.99.1.2", ""), true);
    assert.equal(isPeerAllowed("::1", ""), true);
    assert.equal(isPeerAllowed("::ffff:127.0.0.1", ""), true);
    assert.equal(isPeerAllowed("::ffff:192.168.4.9", "localhost,absolute, 192.168.*.*"), true);
    assert.equal(isPeerAllowed("::ffff:10.1.2.3", "192.168.*.*, 10.1.2.3"), true);
    assert.equal(isPeerAllowed("10.1.2.3", "::ffff:10.1.2.3"), true);
    assert.equal(isPeerAllowed("2001:db8::1", "2001:0DB8:0:0:0:0:0:1"), true);
    assert.equal(isPeerAllowed("10.1.2.3", "localhost,absolute,10.1.*"), false);
    assert.equal(isPeerAllowed("10.1.2.3", "10.1.2.999,not-an-ip"), false);
    assert.equal(isPeerAllowed("10.1.2.3", "10.1.2.003"), false);
    assert.equal(isPeerAllowed("192.168.4.9", "localhost,absolute 192.168.*.*"), false);
    assert.equal(isPeerAllowed(undefined, "*.*.*.*"), false);
});

test("remote server renders profiles and confines same-origin local resources", async (context) => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-remote-"));
    const root = join(directory, "overlay");
    await mkdir(root);
    await writeFile(join(root, "index.html"), "<link rel=stylesheet href=asset.css>", "utf8");
    await writeFile(join(root, "asset.css"), "body{color:pink}", "utf8");
    const canonicalRoot = await realpath(root);
    const token = "a".repeat(32);
    const encodedRoot = Buffer.from(canonicalRoot, "utf8").toString("base64url");
    const localUrl = `vosulay-overlay://${token}/${encodedRoot}/index.html`;
    let config: AppConfig = normalizeConfig({
        active_profile: "active",
        profiles: [
            {
                id: "first",
                name: "First & profile",
                overlays: [{
                    counter_id: "counter/one",
                    top: 12.5,
                    left: -8,
                    width: 640,
                    height: 360,
                    scale: 1.25,
                    opacity: 0.4,
                    rotation: -15,
                    z_index: 7,
                }],
            },
            {
                id: "active",
                name: "Active",
                overlays: [
                    { counter_id: "web", url: "https://example.test/widget?a=1&b=2", top: 1, left: 2, width: 300, height: 200, scale: 1, opacity: 1, rotation: 0, z_index: 2 },
                    { counter_id: "local", url: localUrl, top: 3, left: 4, width: 500, height: 300, scale: 0.5, opacity: 0.75, rotation: 20, z_index: 3 },
                ],
            },
        ],
        settings: { allowed_ips: "" },
    });
    config.profiles[1]?.overlays.push({
        counter_id: "must-not-fallback",
        url: "vosulay-overlay://invalid-token/root/index.html",
        top: 0,
        left: 0,
        width: 100,
        height: 100,
        scale: 1,
        opacity: 1,
        rotation: 0,
        z_index: 4,
    });
    const server = new RemoteOverlayServer({
        runtime,
        registry: new LocalOverlayRegistry(),
        getConfig: async () => config,
        host: "127.0.0.1",
        port: 0,
    });
    await server.start();
    context.after(async () => server.stop());
    const address = server.address();
    assert.notEqual(address, null);
    if (address === null) throw new Error("remote server has no listening address");
    const base = `http://127.0.0.1:${address.port}`;

    const active = await fetch(`${base}/overlay`);
    assert.equal(active.status, 200);
    assert.equal(active.headers.get("cache-control"), "no-store");
    assert.equal(active.headers.get("referrer-policy"), "no-referrer");
    assert.match(active.headers.get("content-security-policy") ?? "", /frame-src 'self' http: https:/);
    assert.match(active.headers.get("content-security-policy") ?? "", /frame-ancestors \*/);
    assert.equal(active.headers.get("x-content-type-options"), "nosniff");
    assert.equal(active.headers.get("x-frame-options"), null);
    assert.equal(active.headers.get("cross-origin-resource-policy"), "cross-origin");
    const activeHtml = await active.text();
    assert.match(activeHtml, /src="https:\/\/example\.test\/widget\?a=1&amp;b=2"/);
    assert.match(activeHtml, /referrerpolicy="no-referrer"/);
    assert.match(activeHtml, new RegExp(`src="/local/${token}/${encodedRoot}/index\\.html"`));
    assert.match(activeHtml, /position:absolute;left:4px;top:3px;width:500px;height:300px;transform-origin:0 0;transform:scale\(0\.5\) rotate\(20deg\);opacity:0\.75;z-index:3/);
    const customFrames = activeHtml.match(/<iframe[^>]+>/g) ?? [];
    assert.equal(customFrames.length, 2);
    for (const frame of customFrames) {
        assert.match(frame, /title="(?:web|local)"/);
        assert.match(frame, /allow="autoplay"/);
        assert.match(frame, /loading="eager"/);
        assert.match(frame, /sandbox="allow-scripts allow-same-origin"/);
    }
    assert.doesNotMatch(activeHtml, /must-not-fallback/);

    const first = await new Promise<{ body: string; status: number | undefined }>((resolve, reject) => {
        const client = httpRequest(`${base}/overlay?profile=1`, { headers: { host: "192.0.2.44:5050" } }, (response) => {
            response.setEncoding("utf8");
            let body = "";
            response.on("data", (chunk: string) => {
                body += chunk;
            });
            response.on("end", () => {
                resolve({ body, status: response.statusCode });
            });
        });
        client.on("error", reject);
        client.end();
    });
    const firstHtml = first.body;
    assert.equal(first.status, 200);
    assert.match(firstHtml, /<title>First &amp; profile<\/title>/);
    assert.match(firstHtml, /src="http:\/\/192\.0\.2\.44:24050\/counter%2Fone\/"/);
    assert.match(firstHtml, /position:absolute;left:-8px;top:12\.5px;width:640px;height:360px;transform-origin:0 0;transform:scale\(1\.25\) rotate\(-15deg\);opacity:0\.4;z-index:7/);
    const standardFrame = firstHtml.match(/<iframe[^>]+>/)?.[0] ?? "";
    assert.match(standardFrame, /title="counter\/one"/);
    assert.match(standardFrame, /allow="autoplay"/);
    assert.match(standardFrame, /loading="eager"/);
    assert.doesNotMatch(standardFrame, /sandbox=/);

    const decoded = await fetch(`${base}/overlay?%70rofile=%31`);
    assert.equal(decoded.status, 200);
    assert.match(await decoded.text(), /<title>First &amp; profile<\/title>/);

    const head = await fetch(`${base}/overlay?profile=2`, { method: "HEAD" });
    assert.equal(head.status, 200);
    assert.equal(await head.text(), "");
    assert.ok(Number(head.headers.get("content-length")) > 0);

    for (const query of [
        "profile=",
        "profile=0",
        "profile=-1",
        "profile=01",
        "profile=1&profile=2",
        "other=1",
        "profile=1&other=2",
        "profile=%",
    ]) {
        const rejected = await fetch(`${base}/overlay?${query}`);
        assert.equal(rejected.status, 400, query);
        assert.match(await rejected.text(), /profile/i);
    }
    const outOfRange = await fetch(`${base}/overlay?profile=3`);
    assert.equal(outOfRange.status, 404);
    assert.match(await outOfRange.text(), /out of range/i);
    const method = await fetch(`${base}/overlay`, { method: "POST" });
    assert.equal(method.status, 405);
    assert.equal(method.headers.get("allow"), "GET, HEAD");

    const local = await fetch(`${base}/local/${token}/${encodedRoot}/asset.css`);
    assert.equal(local.status, 200);
    assert.equal(local.headers.get("content-type"), "text/css; charset=utf-8");
    assert.equal(await local.text(), "body{color:pink}");
    assert.equal(local.headers.get("cross-origin-resource-policy"), "cross-origin");
    const localHead = await fetch(`${base}/local/${token}/${encodedRoot}/asset.css`, { method: "HEAD" });
    assert.equal(localHead.status, 200);
    assert.equal(await localHead.text(), "");
    assert.equal(Number(localHead.headers.get("content-length")), Buffer.byteLength("body{color:pink}"));
    assert.equal((await fetch(`${base}/local/${"b".repeat(32)}/${encodedRoot}/asset.css`)).status, 404);
    assert.equal((await fetch(`${base}/local/${token}/${encodedRoot}/%2e%2e%2fsecret.txt`)).status, 404);

    config = normalizeConfig({
        active_profile: "only",
        profiles: [{ id: "only", name: "Only", overlays: [] }],
        settings: { allowed_ips: "127.0.0.1,localhost,absolute" },
    });
    assert.equal((await fetch(`${base}/local/${token}/${encodedRoot}/asset.css`)).status, 404);
});
