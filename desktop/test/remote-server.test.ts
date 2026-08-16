import {
    RemoteOverlayServer,
    isPeerAllowed,
    renderProfileDocument,
} from "../src/main/remote-server";
import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import type { AppConfig } from "../src/main/types";
import { LocalOverlayRegistry } from "../src/main/local-overlays";
import assert from "node:assert/strict";
import { request as httpRequest } from "node:http";
import { join } from "node:path";
import { normalizeConfig } from "../src/main/config";
import test from "node:test";
import { tmpdir } from "node:os";

const runtime = {
    fpsLimit: 60,
    pids: [],
    processNames: [],
    vosuUrl: new URL("http://127.0.0.1:24050/base/"),
};
const accessToken = "c".repeat(64);
const noop = (): void => {};

test("allowed_ips defaults exactly and preserves the configured peer list", () => {
    assert.equal(normalizeConfig({}).settings.allowed_ips, "127.0.0.1,localhost,absolute");
    assert.equal(
        normalizeConfig({ settings: { allowed_ips: "10.0.*.* ::1" } }).settings.allowed_ips,
        "10.0.*.* ::1",
    );
    assert.equal(
        normalizeConfig({ settings: { allowed_ips: 42 } }).settings.allowed_ips,
        "127.0.0.1,localhost,absolute",
    );
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
    const encodedRoot = Buffer.from(canonicalRoot, "utf8").toString("base64url");
    const localRegistry = new LocalOverlayRegistry();
    const localUrl = await localRegistry.createUrl(join(root, "index.html"));
    const token = new URL(localUrl).hostname;
    let config: AppConfig = normalizeConfig({
        active_profile: "active",
        profiles: [
            {
                id: "first",
                name: "First & profile",
                overlays: [
                    {
                        counter_id: "counter/one",
                        height: 360,
                        left: -8,
                        opacity: 0.4,
                        rotation: -15,
                        scale: 1.25,
                        top: 12.5,
                        width: 640,
                        z_index: 7,
                    },
                ],
            },
            {
                id: "active",
                name: "Active",
                overlays: [
                    {
                        counter_id: "web",
                        height: 200,
                        left: 2,
                        opacity: 1,
                        rotation: 0,
                        scale: 1,
                        top: 1,
                        url: "https://example.test/widget?a=1&b=2",
                        width: 300,
                        z_index: 2,
                    },
                    {
                        counter_id: "local",
                        height: 300,
                        left: 4,
                        opacity: 0.75,
                        rotation: 20,
                        scale: 0.5,
                        top: 3,
                        url: localUrl,
                        width: 500,
                        z_index: 3,
                    },
                ],
            },
        ],
        settings: { allowed_ips: "", remote_access_token: accessToken },
    });
    config.profiles[1]?.overlays.push({
        counter_id: "must-not-fallback",
        height: 100,
        left: 0,
        opacity: 1,
        rotation: 0,
        scale: 1,
        top: 0,
        url: "vosulay-overlay://invalid-token/root/index.html",
        width: 100,
        z_index: 4,
    });
    const server = new RemoteOverlayServer({
        host: "127.0.0.1",
        initialConfig: config,
        port: 0,
        registry: localRegistry,
        runtime,
    });
    await server.start();
    context.after(() => server.stop());
    const address = server.address();
    assert.notEqual(address, undefined);
    if (address === undefined) {
        throw new Error("remote server has no listening address");
    }
    const base = `http://127.0.0.1:${address.port}`;
    const protectedBase = `${base}/access/${accessToken}`;
    const active = await fetch(`${protectedBase}/overlay`);
    assert.equal(active.status, 200);
    assert.equal(active.headers.get("cache-control"), "no-store");
    assert.equal(active.headers.get("referrer-policy"), "no-referrer");
    assert.match(
        active.headers.get("content-security-policy") ?? "",
        /frame-src 'self' http: https:/u,
    );
    assert.match(active.headers.get("content-security-policy") ?? "", /frame-ancestors \*/u);
    assert.equal(active.headers.get("permissions-policy"), "webgpu=*");
    assert.equal(active.headers.get("x-content-type-options"), "nosniff");
    // WHATWG `Headers.get()` returns `null` (not `undefined`) for absent headers.
    // eslint-disable-next-line unicorn/no-null
    assert.equal(active.headers.get("x-frame-options"), null);
    assert.equal(active.headers.get("cross-origin-resource-policy"), "same-origin");
    const activeHtml = await active.text();
    assert.match(activeHtml, /src="https:\/\/example\.test\/widget\?a=1&amp;b=2"/u);
    assert.match(activeHtml, /referrerpolicy="no-referrer"/u);
    assert.match(
        activeHtml,
        new RegExp(`src="/access/${accessToken}/local/${token}/index\\.html"`, "u"),
    );
    assert.doesNotMatch(activeHtml, new RegExp(encodedRoot, "u"));
    const missingHostResponse = await fetch(`${base}/overlay`);
    assert.equal(missingHostResponse.status, 404);
    const invalidTokenResponse = await fetch(`${base}/access/${"d".repeat(64)}/overlay`);
    assert.equal(invalidTokenResponse.status, 403);
    config = normalizeConfig({
        ...config,
        settings: { ...config.settings, remote_access_host: "gaming-pc.local" },
    });
    server.updateConfig(config);
    const configuredHost = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(
            `${protectedBase}/overlay`,
            {
                headers: { host: `gaming-pc.local:${address.port}` },
            },
            (response) => {
                response.resume();
                response.on("end", () => resolve(response.statusCode));
            },
        );
        client.on("error", reject);
        client.end();
    });
    assert.equal(configuredHost, 200);
    const unconfiguredHost = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(
            `${protectedBase}/overlay`,
            {
                headers: { host: `attacker.example:${address.port}` },
            },
            (response) => {
                response.resume();
                response.on("end", () => resolve(response.statusCode));
            },
        );
        client.on("error", reject);
        client.end();
    });
    assert.equal(unconfiguredHost, 403);
    const sameOriginProbe = await fetch(`${protectedBase}/overlay`, {
        headers: { origin: "http://attacker.example" },
    });
    assert.equal(sameOriginProbe.status, 403);
    const crossSiteProbe = await fetch(`${protectedBase}/overlay`, {
        headers: { "sec-fetch-site": "cross-site" },
    });
    assert.equal(crossSiteProbe.status, 403);
    assert.match(
        activeHtml,
        /position:absolute;left:4px;top:3px;width:500px;height:300px;transform-origin:0 0;transform:scale\(0\.5\) rotate\(20deg\);opacity:0\.75;z-index:3/u,
    );
    const customFrames = activeHtml.match(/<iframe[^>]+>/gu) ?? [];
    assert.equal(customFrames.length, 2);
    const localFrame = customFrames.find((frame) => frame.includes(`title="local"`)) ?? "";
    const webFrame = customFrames.find((frame) => frame.includes(`title="web"`)) ?? "";
    for (const frame of customFrames) {
        assert.match(frame, /loading="eager"/u);
    }
    assert.match(localFrame, /allow="autoplay; webgpu"/u);
    assert.match(webFrame, /allow="autoplay"/u);
    assert.doesNotMatch(webFrame, /webgpu/u);
    assert.match(webFrame, /sandbox="allow-scripts allow-same-origin"/u);
    assert.match(localFrame, /sandbox="allow-scripts"/u);
    assert.doesNotMatch(localFrame, /allow-same-origin/u);
    assert.doesNotMatch(activeHtml, /must-not-fallback/u);

    const renderedForLan = renderProfileDocument(
        config.profiles[0]!,
        runtime,
        accessToken,
        "192.0.2.44:5050",
    );
    assert.match(renderedForLan, /<title>First &amp; profile<\/title>/u);
    assert.match(renderedForLan, /src="http:\/\/192\.0\.2\.44:24050\/counter%2Fone\/"/u);
    assert.match(
        renderedForLan,
        /position:absolute;left:-8px;top:12\.5px;width:640px;height:360px;transform-origin:0 0;transform:scale\(1\.25\) rotate\(-15deg\);opacity:0\.4;z-index:7/u,
    );
    const standardFrame = renderedForLan.match(/<iframe[^>]+>/gu)?.[0] ?? "";
    assert.match(standardFrame, /title="counter\/one"/u);
    assert.match(standardFrame, /allow="autoplay; webgpu"/u);
    assert.match(standardFrame, /loading="eager"/u);
    assert.doesNotMatch(standardFrame, /sandbox=/u);
    const rebound = await new Promise<number | undefined>((resolve, reject) => {
        const client = httpRequest(
            `${protectedBase}/overlay?profile=1`,
            {
                headers: { host: "attacker.example:5050" },
            },
            (response) => {
                response.resume();
                response.on("end", () => resolve(response.statusCode));
            },
        );
        client.on("error", reject);
        client.end();
    });
    assert.equal(rebound, 403);

    const decoded = await fetch(`${protectedBase}/overlay?%70rofile=%31`);
    assert.match(await decoded.text(), /<title>First &amp; profile<\/title>/u);
    const head = await fetch(`${protectedBase}/overlay?profile=2`, { method: "HEAD" });
    assert.equal(head.status, 200);
    assert.equal(await head.text(), "");
    assert.ok(Number(head.headers.get("content-length")) > 0);

    await Promise.all(
        [
            "profile=",
            "profile=0",
            "profile=-1",
            "profile=01",
            "profile=1&profile=2",
            "other=1",
            "profile=1&other=2",
            "profile=%",
        ].map(async (query) => {
            const rejected = await fetch(`${protectedBase}/overlay?${query}`);
            assert.equal(rejected.status, 400, query);
            assert.match(await rejected.text(), /profile/iu);
        }),
    );
    const outOfRange = await fetch(`${protectedBase}/overlay?profile=3`);
    assert.equal(outOfRange.status, 404);
    assert.match(await outOfRange.text(), /out of range/iu);
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
    assert.equal(
        Number(localHead.headers.get("content-length")),
        Buffer.byteLength("body{color:pink}"),
    );
    const invalidToken = await fetch(`${protectedBase}/local/${"b".repeat(32)}/asset.css`);
    assert.equal(invalidToken.status, 404);
    const escapedResource = await fetch(`${localBase}/%2e%2e%2fsecret.txt`);
    assert.equal(escapedResource.status, 404);
    assert.equal(
        new URL("asset.css", `${localBase}/index.html`).pathname,
        `/access/${accessToken}/local/${token}/asset.css`,
    );

    config = normalizeConfig({
        active_profile: "only",
        profiles: [{ id: "only", name: "Only", overlays: [] }],
        settings: {
            allowed_ips: "127.0.0.1,localhost,absolute",
            remote_access_token: accessToken,
        },
    });
    server.updateConfig(config);
    const localAfterConfig = await fetch(`${localBase}/asset.css`);
    assert.equal(localAfterConfig.status, 404);
});

test("remote server bounds active handlers before local file work", async (context) => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-remote-admission-"));
    const asset = join(directory, "asset.css");
    await writeFile(asset, "body{}", "utf8");
    const capabilityToken = "e".repeat(32);
    const encodedRoot = Buffer.from(directory, "utf8").toString("base64url");
    const config = normalizeConfig({
        profiles: [
            {
                id: "default",
                name: "Default",
                overlays: [
                    {
                        counter_id: "local",
                        url: `vosulay-overlay://${capabilityToken}/${encodedRoot}/asset.css`,
                    },
                ],
            },
        ],
        settings: { remote_access_token: accessToken },
    });
    let release: () => void = noop;
    const gate = new Promise<void>((resolve) => {
        release = resolve;
    });
    let allStarted: () => void = noop;
    let started = 0;
    const startedGate = new Promise<void>((resolve) => {
        allStarted = resolve;
    });
    const registry = new LocalOverlayRegistry();
    registry.authorizeUrl = (): boolean => true;
    registry.resolveAuthorizedFile = async (): Promise<string> => {
        started += 1;
        if (started === 32) {
            allStarted();
        }
        await gate;
        return asset;
    };
    const server = new RemoteOverlayServer({
        host: "127.0.0.1",
        initialConfig: config,
        port: 0,
        registry,
        runtime,
    });
    await server.start();
    context.after(() => server.stop());
    const address = server.address();
    assert.notEqual(address, undefined);
    if (address === undefined) {
        throw new Error("remote server has no listening address");
    }
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
