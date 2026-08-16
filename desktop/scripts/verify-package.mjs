import { join, resolve } from "node:path";
import { readFile, stat } from "node:fs/promises";
import { extractFile } from "@electron/asar";

const PLATFORM_ARGUMENT_INDEX = 2;
const EXECUTABLE_MODE_MASK = 0o111;

const platform = process.argv[PLATFORM_ARGUMENT_INDEX] ?? "win";
if (platform !== "win" && platform !== "linux") {
    throw new Error(`unsupported package platform: ${platform}`);
}
const resources = resolve(
    "dist",
    platform === "win" ? "win-unpacked" : "linux-unpacked",
    "resources",
);
const packageMetadata = JSON.parse(await readFile(resolve("package.json"), "utf8"));
const artifactNames =
    platform === "win"
        ? [
              `vosulay-${packageMetadata.version}-setup.exe`,
              `vosulay-${packageMetadata.version}-portable.exe`,
          ]
        : [`vosulay-${packageMetadata.version}-x64.AppImage`];

const artifactChecks = await Promise.all(
    artifactNames.map((artifactName) =>
        stat(resolve("dist", artifactName))
            .then((artifactInfo) => ({ artifactInfo, artifactName }))
            .catch(() => ({ artifactInfo: undefined, artifactName })),
    ),
);
for (const { artifactName, artifactInfo } of artifactChecks) {
    if (!artifactInfo?.isFile() || artifactInfo.size === 0) {
        throw new Error(`Packaged ${platform} artifact is missing or empty: ${artifactName}`);
    }
}

const requiredFiles = [
    "app.asar",
    "assets/manager.html",
    "assets/manager.css",
    "assets/manager.js",
    ...(platform === "win"
        ? [
              "app.asar.unpacked/node_modules/@asdf-overlay/core/addon-x64.node",
              "app.asar.unpacked/node_modules/@asdf-overlay/core/asdf_overlay-x64.dll",
              "app.asar.unpacked/node_modules/@asdf-overlay/core/asdf_overlay-x86.dll",
              "app.asar.unpacked/node_modules/@asdf-overlay/electron/lib/input.js",
              "app.asar.unpacked/node_modules/@asdf-overlay/electron/lib/surface.js",
              "app.asar.unpacked/node_modules/@paymoapp/active-window/build/Release/PaymoActiveWindow.node",
          ]
        : [
              "native/overlay-node.linux-x64-gnu.node",
              "native/liboverlay_agent_gl.so",
              "native/liboverlay_dotnet_bootstrap.so",
              "native/liboverlay_vulkan_layer.so",
              "native/VkLayer_VOSULAY_overlay.json",
              "native/dotnet/Vosulay.AttachProfiler",
          ]),
];
const forbiddenFiles =
    platform === "win"
        ? [
              "native/overlay-node.linux-x64-gnu.node",
              "native/liboverlay_agent_gl.so",
              "native/liboverlay_dotnet_bootstrap.so",
              "native/liboverlay_vulkan_layer.so",
              "native/VkLayer_VOSULAY_overlay.json",
          ]
        : [
              "app.asar.unpacked/node_modules/@asdf-overlay/core/addon-x64.node",
              "app.asar.unpacked/node_modules/@asdf-overlay/electron/lib/input.js",
              "app.asar.unpacked/node_modules/@paymoapp/active-window/build/Release/PaymoActiveWindow.node",
          ];

const requiredFileChecks = await Promise.all(
    requiredFiles.map((relativePath) =>
        stat(resolve(resources, relativePath))
            .then((fileInfo) => ({ fileInfo, relativePath }))
            .catch(() => ({ fileInfo: undefined, relativePath })),
    ),
);
for (const { relativePath, fileInfo } of requiredFileChecks) {
    if (!fileInfo?.isFile() || fileInfo.size === 0) {
        throw new Error(`Packaged vosulay resource is missing or empty: ${relativePath}`);
    }
}

const forbiddenFileChecks = await Promise.all(
    forbiddenFiles.map((relativePath) =>
        stat(resolve(resources, relativePath)).then(
            () => ({ exists: true, relativePath }),
            () => ({ exists: false, relativePath }),
        ),
    ),
);
for (const { relativePath, exists } of forbiddenFileChecks) {
    if (exists) {
        throw new Error(
            `Unexpected ${platform === "win" ? "Linux" : "Windows"} resource in package: ${relativePath}`,
        );
    }
}

if (platform === "linux") {
    const attachTool = await stat(resolve(resources, "native/dotnet/Vosulay.AttachProfiler"));
    if ((attachTool.mode & EXECUTABLE_MODE_MASK) === 0) {
        throw new Error("Packaged profiler attach tool is not executable");
    }
}

const preloadPath = "out/preload/index.js";
const packagedPackage = JSON.parse(
    extractFile(resolve(resources, "app.asar"), "package.json").toString("utf8"),
);
if (packagedPackage.name !== "vosulay") {
    throw new Error("Packaged application metadata does not use canonical vosulay identity");
}
const preloadArchivePath = join("out", "preload", "index.js");
const preload = (() => {
    try {
        return extractFile(resolve(resources, "app.asar"), preloadArchivePath);
    } catch (error) {
        throw new Error(`Packaged vosulay preload is missing: ${preloadPath}`, {
            cause: error,
        });
    }
})();
if (preload.length === 0) {
    throw new Error(`Packaged vosulay preload is empty: ${preloadPath}`);
}

console.log(
    `Verified ${requiredFiles.length} ${platform} packaged vosulay resources and ${preloadPath}.`,
);
