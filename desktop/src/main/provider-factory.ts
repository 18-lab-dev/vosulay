import type { ConfigStore } from "./config";
import type { LinuxOverlayProviderDependencies } from "./linux-overlay-provider";
import type { LocalOverlayRegistry } from "./local-overlays";
import type { OverlayManagerDependencies } from "./overlay-manager";
import type { OverlayProvider } from "./overlay-provider";
import type { RuntimeOptions } from "./options";

type SupportedPlatform = "win32" | "linux";

export interface OverlayProviderDependencies {
    windows?: OverlayManagerDependencies;
    linux?: LinuxOverlayProviderDependencies;
}

function supportedPlatform(platform: NodeJS.Platform): SupportedPlatform {
    if (platform === "win32" || platform === "linux") {
        return platform;
    }
    throw new Error(`Unsupported vosulay host platform: ${platform}`);
}

/**
 * Load only the provider for the current host. Platform modules stay behind
 * this boundary so Linux startup cannot evaluate Windows native dependencies.
 */
export async function createOverlayProvider(
    platform: NodeJS.Platform,
    options: RuntimeOptions,
    configStore: ConfigStore,
    overlayRegistry: LocalOverlayRegistry,
    dependencies: OverlayProviderDependencies = {},
): Promise<OverlayProvider> {
    switch (supportedPlatform(platform)) {
        case "win32": {
            // Platform-specific dynamic import keeps asdf/paymo out of Linux's module graph.
            const { WindowsOverlayProvider } = await import("./windows-overlay-provider");
            return new WindowsOverlayProvider(
                options,
                configStore,
                overlayRegistry,
                dependencies.windows,
            );
        }
        case "linux": {
            const { createLinuxOverlayProvider } = await import("./linux-overlay-provider");
            return createLinuxOverlayProvider(
                options,
                configStore,
                overlayRegistry,
                dependencies.linux,
            );
        }
    }
}
