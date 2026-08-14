import assert from "node:assert/strict";
import test from "node:test";

import type { ConfigStore } from "../src/main/config";
import {
    LinuxOverlayProvider,
    createLinuxOverlayProvider,
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
        assert.deepEqual(calls, [{
            executable: "/games/osu.AppImage",
            args: ["--renderer=vulkan"],
            layerDirectory: "/packaged/native",
            agentLibraryPath: "/packaged/native/liboverlay_agent_gl.so",
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
