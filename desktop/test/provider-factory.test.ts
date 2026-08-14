import assert from "node:assert/strict";
import test from "node:test";

import type { ConfigStore } from "../src/main/config";
import {
    LinuxCandidateTracker,
    LinuxOverlayProvider,
    createLinuxOverlayProvider,
    selectCandidatePid,
} from "../src/main/linux-overlay-provider";
import { createOverlayProvider } from "../src/main/provider-factory";
import type { RuntimeOptions } from "../src/main/options";
import type { LocalOverlayRegistry } from "../src/main/local-overlays";
import { WindowsOverlayProvider } from "../src/main/windows-overlay-provider";

const options: RuntimeOptions = {
    vosuUrl: new URL("http://127.0.0.1:24050/"),
    fpsLimit: 60,
    pids: [],
    processNames: ["osu!.exe"],
};

const configStore = {} as ConfigStore;
const registry = {} as LocalOverlayRegistry;

function providerFor(platform: NodeJS.Platform) {
    return createOverlayProvider(platform, options, configStore, registry, {
        linux: {
            loadNative: () => ({
                attach: () => {
                    throw new Error("native attach should not run during selection");
                },
            }),
        },
    });
}

test("selects Linux provider without loading Windows modules", async () => {
    const provider = await providerFor("linux");
    assert.ok(provider instanceof LinuxOverlayProvider);
    assert.equal(provider.visible, true);
    await provider.stop();
});

test("launches Vulkan target through packaged native addon before provider start", async () => {
    const calls: unknown[] = [];
    const handle = {
        pid: 4242,
        destroy: async () => {},
        setVisible: async () => {},
        setEditing: async () => {},
        on(event: string) {
            assert.equal(event, "disconnect");
        },
    };
    const previousRoot = process.env.VOSULAY_NATIVE_ROOT;
    process.env.VOSULAY_NATIVE_ROOT = "/packaged/native";
    try {
        const provider = await createLinuxOverlayProvider(
            {
                ...options,
                vulkanLaunch: {
                    executable: "/games/osu.AppImage",
                    args: ["--renderer=vulkan"],
                },
            },
            configStore,
            registry,
            {
                native: {
                    attach: () => {
                        throw new Error("launch handle should be reused");
                    },
                    launchVulkan: async (launchOptions) => {
                        calls.push(launchOptions);
                        return handle;
                    },
                },
                scheduleScan: () => () => {},
            },
        );
        // The GL agent must not be preloaded here: both backends would
        // authenticate against the same one-shot socket and cookie, and
        // whichever won first would claim the session.
        assert.deepEqual(calls, [{
            executable: "/games/osu.AppImage",
            args: ["--renderer=vulkan"],
            layerDirectory: "/packaged/native",
        }]);
        assert.ok(provider instanceof LinuxOverlayProvider);
        await provider.stop();
    } finally {
        if (previousRoot === undefined) delete process.env.VOSULAY_NATIVE_ROOT;
        else process.env.VOSULAY_NATIVE_ROOT = previousRoot;
    }
});

test("selects Windows provider on win32", async () => {
    const provider = await providerFor("win32");
    assert.ok(provider instanceof WindowsOverlayProvider);
    await provider.stop();
});

test("rejects unsupported host platforms", async () => {
    await assert.rejects(
        createOverlayProvider("darwin", options, configStore, registry),
        /Unsupported vosulay host platform: darwin/,
    );
});

test("candidate selection is stable and deterministic without a compositor signal", () => {
    // Lowest PID wins: the earliest-started match is the game rather than a
    // helper it spawned later.
    assert.equal(selectCandidatePid(new Set([4242, 1337, 9001]), null), 1337);
    // The current choice is kept while it is still running, so the overlay
    // does not flap between two matching processes.
    assert.equal(selectCandidatePid(new Set([4242, 1337]), 4242), 4242);
    // ...but is replaced once it exits.
    assert.equal(selectCandidatePid(new Set([4242]), 1337), 4242);
    assert.equal(selectCandidatePid(new Set(), 4242), null);
});

test("the Linux tracker reports a discovered process as foreground", () => {
    const tracker = new LinuxCandidateTracker();
    const seen: Array<number | null> = [];
    assert.equal(tracker.start((pid) => seen.push(pid)), null);

    // Default startup has no --pid, so discovery is the only foreground signal.
    tracker.observe(new Set([4242]));
    assert.deepEqual(seen, [4242]);

    // No change means no spurious reselection.
    tracker.observe(new Set([4242, 9001]));
    assert.deepEqual(seen, [4242]);

    tracker.observe(new Set([9001]));
    assert.deepEqual(seen, [4242, 9001]);

    tracker.observe(new Set());
    assert.deepEqual(seen, [4242, 9001, null]);
});

test("default Linux startup discovers targets by process name", async () => {
    // The regression: replacing discovery with `options.pids` left the allowed
    // set permanently empty whenever no --pid was supplied.
    const provider = new LinuxOverlayProvider(
        { ...options, pids: [], processNames: ["osu!.exe"] },
        configStore,
        registry,
        {
            native: {
                attach: () => {
                    throw new Error("no process should be attached in this test");
                },
            },
            scheduleScan: () => () => {},
        },
    );
    assert.ok(provider instanceof LinuxOverlayProvider);
    await provider.stop();
});
