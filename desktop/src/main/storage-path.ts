import { isAbsolute, join } from "node:path";

export interface ConfigStoragePaths {
    dataDirectory: string;
    configPath: string;
}

export function resolveStoragePaths(
    appDataDirectory: string,
    portableExecutableDirectory?: string,
): ConfigStoragePaths {
    let dataDirectory: string;
    if (portableExecutableDirectory === undefined) {
        dataDirectory = join(appDataDirectory, "vosulay");
    } else {
        if (portableExecutableDirectory.length === 0) {
            throw new Error("portable executable directory must not be empty");
        }
        if (!isAbsolute(portableExecutableDirectory)) {
            throw new Error("portable executable directory must be absolute");
        }
        dataDirectory = join(portableExecutableDirectory, "data");
    }
    return {
        configPath: join(dataDirectory, "config.json"),
        dataDirectory,
    };
}
