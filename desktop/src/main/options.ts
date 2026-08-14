import { resolve } from "node:path";

export interface VulkanLaunchOptions {
    executable: string;
    args: string[];
}

export interface RuntimeOptions {
    vosuUrl: URL;
    fpsLimit: number;
    pids: number[];
    processNames: string[];
    vulkanLaunch?: VulkanLaunchOptions;
}

export type InstanceCommand = "toggle-overlay";

export interface ParsedInvocation {
    options: RuntimeOptions;
    command: InstanceCommand | null;
}

const DEFAULT_VOSU_URL = "http://127.0.0.1:24050/";
const DEFAULT_FPS_LIMIT = 60;

export function validateBaseUrl(value: string): URL {
    let url: URL;
    try {
        url = new URL(value);
    } catch {
        throw new Error("vosu base URL must be an absolute URL");
    }
    if ((url.protocol !== "http:" && url.protocol !== "https:") || !url.hostname) {
        throw new Error("vosu base URL must use http or https and include a host");
    }
    if (url.username || url.password) {
        throw new Error("vosu base URL must not contain credentials");
    }
    if (url.search || url.hash) {
        throw new Error("vosu base URL must not contain a query or fragment");
    }
    if (!url.pathname.endsWith("/")) {
        url.pathname += "/";
    }
    return url;
}

function positiveInteger(value: string, option: string, maximum: number): number {
    if (!/^[1-9]\d*$/.test(value)) {
        throw new Error(`${option} must be a positive integer`);
    }
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed) || parsed > maximum) {
        throw new Error(`${option} must be at most ${maximum}`);
    }
    return parsed;
}

function optionValue(argv: string[], index: number, long: string, short?: string): [string, number] | undefined {
    const argument = argv[index];
    if (argument === undefined) return undefined;
    if (argument === long || argument === short) {
        if (index + 1 >= argv.length) {
            throw new Error(`${argument} requires a value`);
        }
        return [argv[index + 1]!, index + 1];
    }
    if (argument.startsWith(`${long}=`)) {
        return [argument.slice(long.length + 1), index];
    }
    if (short && argument.startsWith(short) && argument.length > short.length) {
        return [argument.slice(short.length), index];
    }
    return undefined;
}


function parseArguments(
    argv: string[],
    allowInstanceCommand: boolean,
): ParsedInvocation {
    let vosuUrl = validateBaseUrl(DEFAULT_VOSU_URL);
    let fpsLimit = DEFAULT_FPS_LIMIT;
    const pids: number[] = [];
    const processNames: string[] = ["osu!.exe", "osulazer.exe"];
    let vulkanExecutable: string | null = null;
    const vulkanArgs: string[] = [];
    let command: InstanceCommand | null = null;

    for (let index = 0; index < argv.length; index += 1) {
        const argument = argv[index]!;
        if (argument === "--") {
            if (vulkanExecutable === null) {
                throw new Error("-- is only valid after --launch-vulkan");
            }
            vulkanArgs.push(...argv.slice(index + 1));
            break;
        }
        if (
            allowInstanceCommand
            && (argument === "-i" || argument === "--ipc-toggle-overlay")
        ) {
            command = "toggle-overlay";
            continue;
        }
        const url = optionValue(argv, index, "--vosu-url", "-t");
        if (url) {
            if (!url[0]) throw new Error("--vosu-url requires a value");
            vosuUrl = validateBaseUrl(url[0]);
            index = url[1];
            continue;
        }
        const fps = optionValue(argv, index, "--fps-limit", "-f");
        if (fps) {
            fpsLimit = positiveInteger(fps[0], "--fps-limit", 240);
            index = fps[1];
            continue;
        }
        const pid = optionValue(argv, index, "--pid");
        if (pid) {
            pids.push(positiveInteger(pid[0], "--pid", 0xffffffff));
            index = pid[1];
            continue;
        }
        const launch = optionValue(argv, index, "--launch-vulkan");
        if (launch) {
            if (!launch[0] || vulkanExecutable !== null) {
                throw new Error("--launch-vulkan requires exactly one executable");
            }
            vulkanExecutable = launch[0];
            index = launch[1];
            continue;
        }
        const processName = optionValue(argv, index, "--process-name");
        if (processName) {
            const name = processName[0].trim();
            if (!name || name.includes("/") || name.includes("\\") || name.includes("\0")) {
                throw new Error("--process-name must be a non-empty process filename");
            }
            if (processNames.length === 2 && processNames[0] === "osu!.exe") processNames.length = 0;
            processNames.push(name);
            index = processName[1];
            continue;
        }
        throw new Error(`unknown option: ${argv[index]}`);
    }

    return {
        options: {
            vosuUrl,
            fpsLimit,
            pids,
            processNames,
            ...(vulkanExecutable === null
                ? {}
                : { vulkanLaunch: { executable: vulkanExecutable, args: vulkanArgs } }),
        },
        command,
    };
}

export function parseOptions(argv: string[]): RuntimeOptions {
    return parseArguments(argv, false).options;
}

export function parseInvocation(
    argv: string[],
    electronApplicationPath?: string,
): ParsedInvocation {
    if (electronApplicationPath === undefined) return parseArguments(argv, true);
    const expectedPath = resolve(electronApplicationPath);
    const applicationPathIndex = argv.findIndex(
        (argument) => !argument.startsWith("-") && resolve(argument) === expectedPath,
    );
    return parseArguments(
        applicationPathIndex < 0 ? argv : argv.slice(applicationPathIndex + 1),
        true,
    );
}
