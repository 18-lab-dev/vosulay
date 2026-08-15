import { extractFile } from "@electron/asar";
import { readFile, stat } from "node:fs/promises";
import { join, resolve } from "node:path";

const platform = process.argv[2] ?? "win";
if (platform !== "win" && platform !== "linux") {
    throw new Error(`unsupported package platform: ${platform}`);
}
const resources = resolve(
        "dist",
        platform === "win" ? "win-unpacked" : "linux-unpacked",
        "resources",
    ),
    packageMetadata = JSON.parse(await readFile(resolve("package.json"), "utf8")),
    artifactNames =
        platform === "win"
            ? [
                  `vosulay-${packageMetadata.version}-setup.exe`,
                  `vosulay-${packageMetadata.version}-portable.exe`,
              ]
            : [`vosulay-${packageMetadata.version}-x64.AppImage`];
for (const artifactName of artifactNames) {
    const artifact = resolve("dist", artifactName),
        info = await stat(artifact).catch(() => {});
    if (!info?.isFile() || info.size === 0) {
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
    ],
    forbiddenFiles =
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

for (const relativePath of requiredFiles) {
    const path = resolve(resources, relativePath),
        info = await stat(path).catch(() => {});
    if (!info?.isFile() || info.size === 0) {
        throw new Error(`Packaged vosulay resource is missing or empty: ${relativePath}`);
    }
}
for (const relativePath of forbiddenFiles) {
    const path = resolve(resources, relativePath);
    if (
        await stat(path).then(
            () => true,
            () => false,
        )
    ) {
        throw new Error(
            `Unexpected ${platform === "win" ? "Linux" : "Windows"} resource in package: ${relativePath}`,
        );
    }
}
if (platform === "linux") {
    const attachTool = await stat(resolve(resources, "native/dotnet/Vosulay.AttachProfiler"));
    if ((attachTool.mode & 0o111) === 0) {
        throw new Error("Packaged profiler attach tool is not executable");
    }
}

const preloadPath = "out/preload/index.js",
    packagedPackage = JSON.parse(
        extractFile(resolve(resources, "app.asar"), "package.json").toString("utf8"),
    );
if (packagedPackage.name !== "vosulay") {
    throw new Error("Packaged application metadata does not use canonical vosulay identity");
}
const preloadArchivePath = join("out", "preload", "index.js"),
    preload = (() => {
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
