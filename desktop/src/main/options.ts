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
    command: InstanceCommand | undefined;
}

const DEFAULT_FPS_LIMIT = 60;
const DEFAULT_VOSU_URL = "http://127.0.0.1:24050/";
const MAX_FPS_LIMIT = 240;
const MAX_PID = 0xff_ff_ff_ff;
const DEFAULT_PROCESS_NAME = "osu!.exe";
const DEFAULT_PROCESS_NAME_COUNT = 2;
const POSITIVE_INTEGER_PATTERN = /^[1-9]\d*$/u;

export function validateBaseUrl(value: string): URL {
    let url: URL | undefined = undefined;
    try {
        url = new URL(value);
    } catch {
        throw new Error("vosu base URL must be an absolute URL");
    }
    if (url === undefined) {
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
    if (!POSITIVE_INTEGER_PATTERN.test(value)) {
        throw new Error(`${option} must be a positive integer`);
    }
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed) || parsed > maximum) {
        throw new Error(`${option} must be at most ${maximum}`);
    }
    return parsed;
}

function optionValue(
    argv: string[],
    index: number,
    long: string,
    short?: string,
): [string, number] | undefined {
    const argument = argv[index];
    if (argument === undefined) {
        return undefined;
    }
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

function parseArguments(argv: string[], allowInstanceCommand: boolean): ParsedInvocation {
    let vosuUrl = validateBaseUrl(DEFAULT_VOSU_URL);
    let fpsLimit = DEFAULT_FPS_LIMIT;
    const pids: number[] = [];
    const processNames: string[] = [DEFAULT_PROCESS_NAME, "osulazer.exe"];
    let vulkanExecutable: string | undefined = undefined;
    const vulkanArgs: string[] = [];
    let command: InstanceCommand | undefined = undefined;

    for (let index = 0; index < argv.length; index += 1) {
        const argument = argv[index]!;
        if (argument === "--") {
            if (vulkanExecutable === undefined) {
                throw new Error("-- is only valid after --launch-vulkan");
            }
            vulkanArgs.push(...argv.slice(index + 1));
            break;
        }
        if (allowInstanceCommand && (argument === "-i" || argument === "--ipc-toggle-overlay")) {
            command = "toggle-overlay";
            continue;
        }
        const parsedUrl = optionValue(argv, index, "--vosu-url", "-t");
        if (parsedUrl !== undefined) {
            const [value, nextIndex] = parsedUrl;
            if (!value) {
                throw new Error("--vosu-url requires a value");
            }
            vosuUrl = validateBaseUrl(value);
            index = nextIndex;
            continue;
        }
        const parsedFps = optionValue(argv, index, "--fps-limit", "-f");
        if (parsedFps !== undefined) {
            const [value, nextIndex] = parsedFps;
            fpsLimit = positiveInteger(value, "--fps-limit", MAX_FPS_LIMIT);
            index = nextIndex;
            continue;
        }
        const parsedPid = optionValue(argv, index, "--pid");
        if (parsedPid !== undefined) {
            const [value, nextIndex] = parsedPid;
            pids.push(positiveInteger(value, "--pid", MAX_PID));
            index = nextIndex;
            continue;
        }
        const parsedLaunch = optionValue(argv, index, "--launch-vulkan");
        if (parsedLaunch !== undefined) {
            const [executable, nextIndex] = parsedLaunch;
            if (!executable || vulkanExecutable !== undefined) {
                throw new Error("--launch-vulkan requires exactly one executable");
            }
            vulkanExecutable = executable;
            index = nextIndex;
            continue;
        }
        const parsedProcessName = optionValue(argv, index, "--process-name");
        if (parsedProcessName !== undefined) {
            const [rawName, nextIndex] = parsedProcessName;
            const name = rawName.trim();
            if (!name || name.includes("/") || name.includes("\\") || name.includes("\0")) {
                throw new Error("--process-name must be a non-empty process filename");
            }
            if (
                processNames.length === DEFAULT_PROCESS_NAME_COUNT &&
                processNames[0] === DEFAULT_PROCESS_NAME
            ) {
                processNames.length = 0;
            }
            processNames.push(name);
            index = nextIndex;
            continue;
        }
        throw new Error(`unknown option: ${argv[index]}`);
    }

    const options: RuntimeOptions = {
        fpsLimit,
        pids,
        processNames,
        vosuUrl,
    };
    if (vulkanExecutable !== undefined) {
        options.vulkanLaunch = {
            args: vulkanArgs,
            executable: vulkanExecutable,
        };
    }
    return {
        command,
        options,
    };
}

export function parseOptions(argv: string[]): RuntimeOptions {
    return parseArguments(argv, false).options;
}

export function parseInvocation(
    argv: string[],
    electronApplicationPath?: string,
): ParsedInvocation {
    if (electronApplicationPath === undefined) {
        return parseArguments(argv, true);
    }
    const expectedPath = resolve(electronApplicationPath);
    const applicationPathIndex = argv.findIndex(
        (argument) => !argument.startsWith("-") && resolve(argument) === expectedPath,
    );
    return parseArguments(
        applicationPathIndex === -1 ? argv : argv.slice(applicationPathIndex + 1),
        true,
    );
}
