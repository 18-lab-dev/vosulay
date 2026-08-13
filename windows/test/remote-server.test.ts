import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { normalizeConfig } from "../src/main/config";
import { LocalOverlayRegistry } from "../src/main/local-overlays";
import { isPeerAllowed, RemoteOverlayServer, renderProfileDocument } from "../src/main/remote-server";
import type { AppConfig } from "../src/main/types";

const runtime = {
    vosuUrl: new URL("http://127.0.0.1:24050/base/"),
    fpsLimit: 60,
    pids: [],
    processNames: [],
};
const accessToken = "c".repeat(64);

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
        settings: { allowed_ips: "", remote_access_token: accessToken },
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
        initialConfig: config,
        host: "127.0.0.1",
        port: 0,
    });
    await server.start();
    context.after(async () => server.stop());
    const address = server.address();
    assert.notEqual(address, null);
    if (address === null) throw new Error("remote server has no listening address");
    const base = `http://127.0.0.1:${address.port}`;
    const protectedBase = `${base}/access/${accessToken}`;

    const active = await fetch(`${protectedBase}/overlay`);
    assert.equal(active.status, 200);
    assert.equal(active.headers.get("cache-control"), "no-store");
    assert.equal(active.headers.get("referrer-policy"), "no-referrer");
    assert.match(active.headers.get("content-security-policy") ?? "", /frame-src 'self' http: https:/);
    assert.match(active.headers.get("content-security-policy") ?? "", /frame-ancestors \*/);
    assert.equal(active.headers.get("permissions-policy"), "webgpu=*");
    assert.equal(active.headers.get("x-content-type-options"), "nosniff");
    assert.equal(active.headers.get("x-frame-options"), null);
    assert.equal(active.headers.get("cross-origin-resource-policy"), "same-origin");
    const activeHtml = await active.text();
    assert.match(activeHtml, /src="https:\/\/example\.test\/widget\?a=1&amp;b=2"/);
    assert.match(activeHtml, /referrerpolicy="no-referrer"/);
    assert.match(activeHtml, new RegExp(`src="/access/${accessToken}/local/${token}/index\\.html"`));
    assert.doesNotMatch(activeHtml, new RegExp(encodedRoot));
    assert.equal((await fetch(`${base}/overlay`)).status, 404);
    assert.equal((await fetch(`${base}/access/${"d".repeat(64)}/overlay`)).status, 403);
    config = normalizeConfig({
        ...config,
        settings: { ...config.settings, remote_access_host: "gaming-pc.local" },
    });
    server.updateConfig(config);
    const configuredHost = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(protectedBase + "/overlay", {
            headers: { host: `gaming-pc.local:${address.port}` },
        }, (response) => {
            response.resume();
            response.on("end", () => resolve(response.statusCode));
        });
        client.on("error", reject);
        client.end();
    });
    assert.equal(configuredHost, 200);
    const unconfiguredHost = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(protectedBase + "/overlay", {
            headers: { host: `attacker.example:${address.port}` },
        }, (response) => {
            response.resume();
            response.on("end", () => resolve(response.statusCode));
        });
        client.on("error", reject);
        client.end();
    });
    assert.equal(unconfiguredHost, 403);
    assert.equal((await fetch(`${protectedBase}/overlay`, {
        headers: { origin: "http://attacker.example" },
    })).status, 403);
    assert.equal((await fetch(`${protectedBase}/overlay`, {
        headers: { "sec-fetch-site": "cross-site" },
    })).status, 403);
    assert.match(activeHtml, /position:absolute;left:4px;top:3px;width:500px;height:300px;transform-origin:0 0;transform:scale\(0\.5\) rotate\(20deg\);opacity:0\.75;z-index:3/);
    const customFrames = activeHtml.match(/<iframe[^>]+>/g) ?? [];
    assert.equal(customFrames.length, 2);
    const webFrame = customFrames.find((frame) => frame.includes(`title="web"`)) ?? "";
    const localFrame = customFrames.find((frame) => frame.includes(`title="local"`)) ?? "";
    for (const frame of customFrames) {
        assert.match(frame, /allow="autoplay; webgpu"/);
        assert.match(frame, /loading="eager"/);
    }
    assert.match(webFrame, /sandbox="allow-scripts allow-same-origin"/);
    assert.match(localFrame, /sandbox="allow-scripts"/);
    assert.doesNotMatch(localFrame, /allow-same-origin/);
    assert.doesNotMatch(activeHtml, /must-not-fallback/);

    const renderedForLan = renderProfileDocument(config.profiles[0]!, runtime, accessToken, "192.0.2.44:5050");
    assert.match(renderedForLan, /<title>First &amp; profile<\/title>/);
    assert.match(renderedForLan, /src="http:\/\/192\.0\.2\.44:24050\/counter%2Fone\/"/);
    assert.match(renderedForLan, /position:absolute;left:-8px;top:12\.5px;width:640px;height:360px;transform-origin:0 0;transform:scale\(1\.25\) rotate\(-15deg\);opacity:0\.4;z-index:7/);
    const standardFrame = renderedForLan.match(/<iframe[^>]+>/)?.[0] ?? "";
    assert.match(standardFrame, /title="counter\/one"/);
    assert.match(standardFrame, /allow="autoplay; webgpu"/);
    assert.match(standardFrame, /loading="eager"/);
    assert.doesNotMatch(standardFrame, /sandbox=/);

    const rebound = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(`${protectedBase}/overlay?profile=1`, { headers: { host: "attacker.example:5050" } }, (response) => {
            response.resume();
            response.on("end", () => resolve(response.statusCode));
        });
        client.on("error", reject);
        client.end();
    });
    assert.equal(rebound, 403);

    const decoded = await fetch(`${protectedBase}/overlay?%70rofile=%31`);
    assert.equal(decoded.status, 200);
    assert.match(await decoded.text(), /<title>First &amp; profile<\/title>/);

    const head = await fetch(`${protectedBase}/overlay?profile=2`, { method: "HEAD" });
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
        const rejected = await fetch(`${protectedBase}/overlay?${query}`);
        assert.equal(rejected.status, 400, query);
        assert.match(await rejected.text(), /profile/i);
    }
    const outOfRange = await fetch(`${protectedBase}/overlay?profile=3`);
    assert.equal(outOfRange.status, 404);
    assert.match(await outOfRange.text(), /out of range/i);
    const method = await fetch(`${protectedBase}/overlay`, { method: "POST" });
    assert.equal(method.status, 405);
    assert.equal(method.headers.get("allow"), "GET, HEAD");

    const localBase = `${protectedBase}/local/${token}`;
    const local = await fetch(`${localBase}/asset.css`);
    assert.equal(local.status, 200);
    assert.equal(local.headers.get("content-type"), "text/css; charset=utf-8");
    assert.equal(await local.text(), "body{color:pink}");
    assert.equal(local.headers.get("cross-origin-resource-policy"), "cross-origin");
    assert.equal(local.headers.get("access-control-allow-origin"), "null");
    const opaqueFrameResource = await fetch(`${localBase}/asset.css`, {
        headers: { origin: "null", "sec-fetch-site": "cross-site" },
    });
    assert.equal(opaqueFrameResource.status, 200);
    assert.equal(opaqueFrameResource.headers.get("cross-origin-resource-policy"), "cross-origin");
    assert.equal(opaqueFrameResource.headers.get("access-control-allow-origin"), "null");
    const localHead = await fetch(`${localBase}/asset.css`, { method: "HEAD" });
    assert.equal(localHead.status, 200);
    assert.equal(await localHead.text(), "");
    assert.equal(Number(localHead.headers.get("content-length")), Buffer.byteLength("body{color:pink}"));
    assert.equal((await fetch(`${protectedBase}/local/${"b".repeat(32)}/asset.css`)).status, 404);
    assert.equal((await fetch(`${localBase}/%2e%2e%2fsecret.txt`)).status, 404);
    assert.equal(new URL("asset.css", `${localBase}/index.html`).pathname, `/access/${accessToken}/local/${token}/asset.css`);

    config = normalizeConfig({
        active_profile: "only",
        profiles: [{ id: "only", name: "Only", overlays: [] }],
        settings: {
            allowed_ips: "127.0.0.1,localhost,absolute",
            remote_access_token: accessToken,
        },
    });
    server.updateConfig(config);
    assert.equal((await fetch(`${localBase}/asset.css`)).status, 404);
});

test("remote server bounds active handlers before local file work", async (context) => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-remote-admission-"));
    const asset = join(directory, "asset.css");
    await writeFile(asset, "body{}", "utf8");
    const capabilityToken = "e".repeat(32);
    const encodedRoot = Buffer.from(directory, "utf8").toString("base64url");
    const config = normalizeConfig({
        profiles: [{
            id: "default",
            name: "Default",
            overlays: [{
                counter_id: "local",
                url: `vosulay-overlay://${capabilityToken}/${encodedRoot}/asset.css`,
            }],
        }],
        settings: { remote_access_token: accessToken },
    });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
        release = resolve;
    });
    let started = 0;
    let allStarted!: () => void;
    const startedGate = new Promise<void>((resolve) => {
        allStarted = resolve;
    });
    const registry = {
        authorizeUrl: () => true,
        resolveAuthorizedFile: async () => {
            started += 1;
            if (started === 32) allStarted();
            await gate;
            return asset;
        },
    } as unknown as LocalOverlayRegistry;
    const server = new RemoteOverlayServer({
        runtime,
        registry,
        initialConfig: config,
        host: "127.0.0.1",
        port: 0,
    });
    await server.start();
    context.after(async () => server.stop());
    const address = server.address();
    assert.notEqual(address, null);
    if (address === null) throw new Error("remote server has no listening address");
    const url = `http://127.0.0.1:${address.port}/access/${accessToken}/local/${capabilityToken}/asset.css`;
    const pending = Array.from({ length: 32 }, () => fetch(url));
    await startedGate;

    const overloaded = await fetch(url);
    assert.equal(overloaded.status, 503);
    assert.equal(started, 32);

    release();
    const responses = await Promise.all(pending);
    assert.equal(responses.filter((response) => response.status === 200).length, 32);
});
