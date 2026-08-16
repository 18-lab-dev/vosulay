import { isAbsolute, join } from "node:path";

export interface ConfigStoragePaths {
    dataDirectory: string;
    configPath: string;
}

export function resolveStoragePaths(
    appDataDirectory: string,
    portableExecutableDirectory?: string,
): ConfigStoragePaths {
    const dataDirectory =
        portableExecutableDirectory === undefined
            ? join(appDataDirectory, "vosulay")
            : resolvePortableExecutableDirectory(portableExecutableDirectory);
    return {
        configPath: join(dataDirectory, "config.json"),
        dataDirectory,
    };
}

function resolvePortableExecutableDirectory(portableExecutableDirectory: string): string {
    if (portableExecutableDirectory.length === 0) {
        throw new Error("portable executable directory must not be empty");
    }
    if (!isAbsolute(portableExecutableDirectory)) {
        throw new Error("portable executable directory must be absolute");
    }
    return join(portableExecutableDirectory, "data");
}
