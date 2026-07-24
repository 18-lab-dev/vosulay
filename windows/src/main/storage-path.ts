import { isAbsolute, join } from "node:path";

export interface ConfigStoragePaths {
    dataDirectory: string;
    configPath: string;
}

export function resolveStoragePaths(
    appDataDirectory: string,
    portableExecutableDirectory?: string
): ConfigStoragePaths {
    let dataDirectory: string;
    if (portableExecutableDirectory !== undefined) {
        if (portableExecutableDirectory.length === 0) {
            throw new Error("portable executable directory must not be empty");
        }
        if (!isAbsolute(portableExecutableDirectory)) {
            throw new Error("portable executable directory must be absolute");
        }
        dataDirectory = join(portableExecutableDirectory, "data");
    } else {
        dataDirectory = join(appDataDirectory, "vosulay");
    }
    return {
        dataDirectory,
        configPath: join(dataDirectory, "config.json"),
    };
}
