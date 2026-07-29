import { extractFile } from "@electron/asar";
import { stat } from "node:fs/promises";
import { join, resolve } from "node:path";

const resources = resolve("dist", "win-unpacked", "resources");
const requiredFiles = [
    "app.asar",
    "assets/manager.html",
    "assets/manager.css",
    "assets/manager.js",
    "app.asar.unpacked/node_modules/@asdf-overlay/core/addon-x64.node",
    "app.asar.unpacked/node_modules/@asdf-overlay/core/asdf_overlay-x64.dll",
    "app.asar.unpacked/node_modules/@asdf-overlay/core/asdf_overlay-x86.dll",
    "app.asar.unpacked/node_modules/@paymoapp/active-window/build/Release/PaymoActiveWindow.node",
];

for (const relativePath of requiredFiles) {
    const path = resolve(resources, relativePath);
    const info = await stat(path).catch(() => null);
    if (!info?.isFile() || info.size === 0) {
        throw new Error(`Packaged vosulay resource is missing or empty: ${relativePath}`);
    }
}

const preloadPath = "out/preload/index.js";
const preloadArchivePath = join("out", "preload", "index.js");
const preload = (() => {
    try {
        return extractFile(resolve(resources, "app.asar"), preloadArchivePath);
    } catch (error) {
        throw new Error(`Packaged vosulay preload is missing: ${preloadPath}`, { cause: error });
    }
})();
if (preload.length === 0) {
    throw new Error(`Packaged vosulay preload is empty: ${preloadPath}`);
}

console.log(`Verified ${requiredFiles.length} packaged vosulay resources and ${preloadPath}.`);
