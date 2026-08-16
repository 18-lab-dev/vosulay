import { basename, join, resolve } from "node:path";
import { cpSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";

const X64_LINUX_PLATFORM = "linux";
const X64_ARCH = "x64";

const desktopRoot = resolve(import.meta.dirname, "..");
const nativeRoot = resolve(desktopRoot, "../native");
const stageRoot = join(desktopRoot, "build/native");
const publishRoot = join(nativeRoot, "target/vosulay-attach-publish");

function run(command, args, cwd) {
    const result = spawnSync(command, args, { cwd, stdio: "inherit" });
    if (result.error) {
        throw result.error;
    }
    if (result.status !== 0) {
        throw new Error(`${command} ${args.join(" ")} exited ${result.status}`);
    }
}

function requireFile(path) {
    const info = statSync(path, { throwIfNoEntry: false });
    if (!info?.isFile() || info.size === 0) {
        throw new Error(`required native artifact is missing or empty: ${path}`);
    }
}

if (process.platform !== X64_LINUX_PLATFORM || process.arch !== X64_ARCH) {
    throw new Error("Linux native staging currently supports linux-x64 only");
}

run("bun", ["run", "build"], nativeRoot);
run(
    "cargo",
    [
        "build",
        "--release",
        "-p",
        "overlay-agent-gl",
        "-p",
        "overlay-dotnet-bootstrap",
        "-p",
        "overlay-vulkan-layer",
    ],
    nativeRoot,
);
rmSync(publishRoot, { force: true, recursive: true });
run(
    "dotnet",
    [
        "publish",
        "dotnet/Vosulay.AttachProfiler/Vosulay.AttachProfiler.csproj",
        "-c",
        "Release",
        "-r",
        "linux-x64",
        "--self-contained",
        "true",
        "-p:PublishSingleFile=true",
        "-p:DebugType=None",
        "-o",
        publishRoot,
    ],
    join(nativeRoot, "crates/overlay-dotnet-bootstrap"),
);

const artifacts = [
    join(nativeRoot, "crates/overlay-node/overlay-node.linux-x64-gnu.node"),
    join(nativeRoot, "target/release/liboverlay_agent_gl.so"),
    join(nativeRoot, "target/release/liboverlay_dotnet_bootstrap.so"),
    join(nativeRoot, "target/release/liboverlay_vulkan_layer.so"),
    join(nativeRoot, "VkLayer_VOSULAY_overlay.json"),
];
for (const artifact of artifacts) {
    requireFile(artifact);
}
const attachFiles = readdirSync(publishRoot).map((name) => join(publishRoot, name));
if (attachFiles.length === 0) {
    throw new Error("profiler attach publish output is empty");
}
for (const artifact of attachFiles) {
    requireFile(artifact);
}

rmSync(stageRoot, { force: true, recursive: true });
mkdirSync(join(stageRoot, "dotnet"), { recursive: true });
for (const artifact of artifacts) {
    cpSync(artifact, join(stageRoot, basename(artifact)));
}
for (const artifact of attachFiles) {
    cpSync(artifact, join(stageRoot, "dotnet", basename(artifact)));
}
console.log(
    `Staged ${artifacts.length + attachFiles.length} Linux native artifacts in ${stageRoot}`,
);
