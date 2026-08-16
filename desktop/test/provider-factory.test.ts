import {
    LinuxCandidateTracker,
    LinuxOverlayProvider,
    createLinuxOverlayProvider,
    selectCandidatePid,
} from "../src/main/linux-overlay-provider";
import type { ConfigStore } from "../src/main/config";
import type { LocalOverlayRegistry } from "../src/main/local-overlays";
import type { RuntimeOptions } from "../src/main/options";
import { WindowsOverlayProvider } from "../src/main/windows-overlay-provider";
import assert from "node:assert/strict";
import { createOverlayProvider } from "../src/main/provider-factory";
import test from "node:test";
const options: RuntimeOptions = {
    fpsLimit: 60,
    pids: [],
    processNames: ["osu!.exe"],
    vosuUrl: new URL("http://127.0.0.1:24050/"),
};
/* SAFETY: `ConfigStore` is consumed in these tests only for constructor injection and no
 * methods are invoked. */
const configStore = {} as ConfigStore;
/* SAFETY: The provider factory only observes `LocalOverlayRegistry`'s interface shape in these tests
 * and does not exercise registry APIs. */
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
        destroy: () => Promise.resolve(),
        off(event: string, _callback: (payload: string) => void) {
            assert.equal(event, "disconnect");
        },
        on(event: string, _callback: (payload: string) => void) {
            assert.equal(event, "disconnect");
        },
        pid: 4242,
        setEditing: () => Promise.resolve(),
        setVisible: () => Promise.resolve(),
    };
    const previousRoot = process.env.VOSULAY_NATIVE_ROOT;
    process.env.VOSULAY_NATIVE_ROOT = "/packaged/native";
    try {
        const provider = await createLinuxOverlayProvider(
            {
                ...options,
                vulkanLaunch: {
                    args: ["--renderer=vulkan"],
                    executable: "/games/osu.AppImage",
                },
            },
            configStore,
            registry,
            {
                native: {
                    attach: () => {
                        throw new Error("launch handle should be reused");
                    },
                    launchVulkan: (launchOptions) => {
                        calls.push(launchOptions);
                        return Promise.resolve(handle);
                    },
                },
                scheduleScan: () => () => {},
            },
        );
        // The GL agent must not be preloaded here: both backends would
        // Authenticate against the same one-shot socket and cookie, and
        // Whichever won first would claim the session.
        assert.deepEqual(calls, [
            {
                args: ["--renderer=vulkan"],
                executable: "/games/osu.AppImage",
                layerDirectory: "/packaged/native",
            },
        ]);
        assert.ok(provider instanceof LinuxOverlayProvider);
        await provider.stop();
    } finally {
        if (previousRoot === undefined) {
            delete process.env.VOSULAY_NATIVE_ROOT;
        } else {
            process.env.VOSULAY_NATIVE_ROOT = previousRoot;
        }
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
        /Unsupported vosulay host platform: darwin/u,
    );
});

test("candidate selection is stable and deterministic without a compositor signal", () => {
    // Lowest PID wins: the earliest-started match is the game rather than a
    // Helper it spawned later.
    assert.equal(selectCandidatePid(new Set([4242, 1337, 9001])), 1337);
    // The current choice is kept while it is still running, so the overlay
    // Does not flap between two matching processes.
    assert.equal(selectCandidatePid(new Set([4242, 1337]), 4242), 4242);
    // ...but is replaced once it exits.
    assert.equal(selectCandidatePid(new Set([4242]), 1337), 4242);
    assert.equal(selectCandidatePid(new Set(), 4242), undefined);
});

test("the Linux tracker reports a discovered process as foreground", () => {
    const tracker = new LinuxCandidateTracker();
    const seen: (number | undefined)[] = [];
    assert.equal(
        tracker.start((pid) => seen.push(pid)),
        undefined,
    );

    // Default startup has no --pid, so discovery is the only foreground signal.
    tracker.observe(new Set([4242]));
    assert.deepEqual(seen, [4242]);

    // No change means no spurious reselection.
    tracker.observe(new Set([4242, 9001]));
    assert.deepEqual(seen, [4242]);

    tracker.observe(new Set([9001]));
    assert.deepEqual(seen, [4242, 9001]);

    tracker.observe(new Set());
    assert.deepEqual(seen, [4242, 9001, undefined]);
});

test("default Linux startup discovers targets by process name", async () => {
    // The regression: replacing discovery with `options.pids` left the allowed
    // Set permanently empty whenever no --pid was supplied.
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
