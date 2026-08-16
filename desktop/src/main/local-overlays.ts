import { access, realpath, stat } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, parse, relative, sep } from "node:path";
import {
    closeSync,
    constants,
    createReadStream,
    openSync,
    readFileSync,
    realpathSync,
    statSync,
    writeSync,
} from "node:fs";
import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { Readable } from "node:stream";
import type { RuntimeOptions } from "./options";
import { buildManagerDocument } from "./manager-document";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import mime from "mime-types";

const require = createRequire(import.meta.url);
const CAPABILITY_KEY_BYTES = 32;
const CAPABILITY_KEY_TOKEN_BYTES = CAPABILITY_KEY_BYTES;
const CAPABILITY_KEY_FILE_MODE = 0o600;
const CAPABILITY_KEY_FILENAME = "local-overlay-capability.key";
const MINIMUM_OVERLAY_PATH_SEGMENTS = 3;
const OVERLAY_ROOT_SEGMENT_INDEX = 1;
const OVERLAY_FILE_SEGMENT_INDEX = 2;
const ROOT_PATTERN = /^[A-Za-z0-9_-]+$/u;
const TOKEN_PATTERN = /^[0-9a-f]{32}$/u;
const ENOENT_ERROR_CODE = "ENOENT";
const EEXIST_ERROR_CODE = "EEXIST";

interface SchemePrivileges {
    secure: boolean;
    standard: boolean;
    stream?: boolean;
    supportFetchAPI: boolean;
}
interface PrivilegedScheme {
    privileges: SchemePrivileges;
    scheme: string;
}

interface ElectronProtocol {
    registerSchemesAsPrivileged: (schemes: PrivilegedScheme[]) => void;
    handle: (scheme: string, handler: (request: Request) => Promise<Response> | Response) => void;
}

function electronProtocol(): ElectronProtocol {
    return require("electron").protocol;
}

interface ParsedErrnoException {
    code: string;
}

interface ErrorCodeCarrier {
    code: unknown;
}

function hasErrorCode(cause: unknown): cause is ParsedErrnoException {
    if (typeof cause !== "object" || cause === null || !("code" in cause)) {
        return false;
    }
    /* SAFETY: `cause` is checked for a `code` property before narrowing. */
    return typeof (cause as ErrorCodeCarrier).code === "string";
}

interface ParsedOverlayUrl {
    token: string;
    root: string;
    segments: string[];
}

function capabilityKeyPath(dataDirectory: string): string {
    if (!isAbsolute(dataDirectory)) {
        throw new Error("local overlay data directory must be absolute");
    }
    return join(dataDirectory, CAPABILITY_KEY_FILENAME);
}

/**
 * Load or create the installation-local key used to authenticate filesystem
 * roots embedded in `vosulay-overlay:` URLs. The key is deliberately stored
 * outside profile JSON and remote-access URLs, so importing a profile cannot
 * mint a new readable-root capability.
 */
export function loadLocalOverlayCapabilityKey(dataDirectory: string): Buffer {
    const path = capabilityKeyPath(dataDirectory);
    try {
        const key = readFileSync(path);
        if (key.length !== CAPABILITY_KEY_BYTES) {
            throw new Error("local overlay capability key has an invalid length");
        }
        return key;
    } catch (error) {
        if (!hasErrorCode(error) || error.code !== ENOENT_ERROR_CODE) {
            throw error;
        }
    }

    const key = randomBytes(CAPABILITY_KEY_BYTES);
    let descriptor = -1;
    try {
        descriptor = openSync(
            path,
            constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
            CAPABILITY_KEY_FILE_MODE,
        );
        let offset = 0;
        while (offset < key.length) {
            offset += writeSync(descriptor, key, offset, key.length - offset);
        }
        return key;
    } catch (error) {
        if (hasErrorCode(error) && error.code === EEXIST_ERROR_CODE) {
            return loadLocalOverlayCapabilityKey(dataDirectory);
        }
        throw error;
    } finally {
        if (descriptor !== -1) {
            closeSync(descriptor);
        }
    }
}

function rootToken(key: Buffer, root: string): string {
    return createHmac("sha256", key)
        .update("vosulay-local-overlay-root\0", "utf8")
        .update(root, "utf8")
        .digest("hex")
        .slice(0, CAPABILITY_KEY_TOKEN_BYTES);
}

function tokenMatches(left: string, right: string): boolean {
    if (!TOKEN_PATTERN.test(left) || !TOKEN_PATTERN.test(right)) {
        return false;
    }
    return timingSafeEqual(Buffer.from(left, "ascii"), Buffer.from(right, "ascii"));
}

function decodeRoot(segment: string): string {
    if (!ROOT_PATTERN.test(segment) || segment.includes("=")) {
        throw new Error("invalid local overlay root encoding");
    }
    const bytes = Buffer.from(segment, "base64url");
    if (bytes.toString("base64url") !== segment) {
        throw new Error("non-canonical local overlay root encoding");
    }
    const root = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (!root || !isAbsolute(root) || Buffer.from(root, "utf8").toString("base64url") !== segment) {
        throw new Error("invalid local overlay root");
    }
    try {
        if (realpathSync.native(root) !== root || !statSync(root).isDirectory()) {
            throw new Error("non-canonical local overlay root");
        }
    } catch {
        throw new Error("local overlay root is not a canonical readable directory");
    }
    // A user selecting one HTML file should not accidentally grant the whole
    // Filesystem or home directory to every asset path below that capability.
    if (root === parse(root).root || root === realpathSync.native(homedir())) {
        throw new Error("local overlay root is too broad");
    }
    return root;
}

function parseOverlayUrl(value: URL | string): ParsedOverlayUrl {
    const url = value instanceof URL ? value : new URL(value);
    const encoded = url.pathname.split("/");
    if (
        url.protocol.toLowerCase() !== "vosulay-overlay:" ||
        !TOKEN_PATTERN.test(url.hostname) ||
        url.port ||
        url.username ||
        url.password
    ) {
        throw new Error("invalid local overlay URL");
    }
    if (
        encoded.length < MINIMUM_OVERLAY_PATH_SEGMENTS ||
        encoded[0] !== "" ||
        !encoded[OVERLAY_ROOT_SEGMENT_INDEX]
    ) {
        throw new Error("invalid local overlay path");
    }
    const root = encoded[OVERLAY_ROOT_SEGMENT_INDEX];
    const segments = encoded.slice(OVERLAY_FILE_SEGMENT_INDEX).map((segment) => {
        let decoded = "";
        try {
            decoded = decodeURIComponent(segment);
        } catch {
            throw new Error("invalid local overlay path encoding");
        }
        if (
            !decoded ||
            decoded === "." ||
            decoded === ".." ||
            decoded.includes("/") ||
            decoded.includes("\\") ||
            decoded.includes("\0")
        ) {
            throw new Error("unsafe local overlay path");
        }
        return decoded;
    });
    return { root: decodeRoot(root), segments, token: url.hostname };
}

function confined(root: string, target: string): boolean {
    const path = relative(root, target);
    return path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path);
}

export class LocalOverlayRegistry {
    private readonly roots = new Map<string, string>();
    private readonly capabilityKey: Buffer;

    constructor(capabilityKey: Uint8Array = randomBytes(CAPABILITY_KEY_BYTES)) {
        if (capabilityKey.byteLength !== CAPABILITY_KEY_BYTES) {
            throw new Error("local overlay capability key must contain 32 bytes");
        }
        this.capabilityKey = Buffer.from(capabilityKey);
    }

    async createUrl(filePath: string): Promise<string> {
        const file = await realpath(filePath);
        const info = await stat(file);
        await access(file);
        if (!info.isFile()) {
            throw new Error("selected overlay is not a regular file");
        }
        const root = await realpath(dirname(file));
        // Reuse decodeRoot's canonical and broad-root checks before minting.
        const encodedRoot = Buffer.from(root, "utf8").toString("base64url");
        decodeRoot(encodedRoot);
        const token = rootToken(this.capabilityKey, root);
        const url = `vosulay-overlay://${token}/${encodedRoot}/${encodeURIComponent(basename(file))}`;
        if (!this.authorizeUrl(url)) {
            throw new Error("selected overlay could not be authorized safely");
        }
        return url;
    }

    authorizeUrl(value: URL | string): boolean {
        try {
            const parsed = parseOverlayUrl(value);
            if (!tokenMatches(parsed.token, rootToken(this.capabilityKey, parsed.root))) {
                return false;
            }
            const existing = this.roots.get(parsed.token);
            if (existing !== undefined && existing !== parsed.root) {
                return false;
            }
            this.roots.set(parsed.token, parsed.root);
            return true;
        } catch {
            return false;
        }
    }

    authorizeUrls(values: readonly (URL | string)[]): boolean {
        const additions = new Map<string, string>();
        try {
            for (const value of values) {
                const parsed = parseOverlayUrl(value);
                if (!tokenMatches(parsed.token, rootToken(this.capabilityKey, parsed.root))) {
                    return false;
                }
                const existing = this.roots.get(parsed.token) ?? additions.get(parsed.token);
                if (existing !== undefined && existing !== parsed.root) {
                    return false;
                }
                additions.set(parsed.token, parsed.root);
            }
        } catch {
            return false;
        }
        for (const [token, root] of additions) {
            this.roots.set(token, root);
        }
        return true;
    }

    async resolveAuthorizedFile(value: URL | string): Promise<string> {
        const parsed = parseOverlayUrl(value);
        if (!tokenMatches(parsed.token, rootToken(this.capabilityKey, parsed.root))) {
            throw new Error("local overlay URL has an invalid capability");
        }
        const authorized = this.roots.get(parsed.token);
        if (authorized === undefined || authorized !== parsed.root) {
            throw new Error("local overlay URL is not authorized");
        }
        const canonicalRoot = await realpath(parsed.root);
        if (canonicalRoot !== parsed.root || canonicalRoot !== authorized) {
            throw new Error("local overlay root is no longer canonical");
        }
        const target = await realpath([canonicalRoot, ...parsed.segments].join(sep));
        if (!confined(canonicalRoot, target)) {
            throw new Error("local overlay path escapes its root");
        }
        const info = await stat(target);
        if (!info.isFile()) {
            throw new Error("local overlay target is not a regular file");
        }
        await access(target);
        return target;
    }

    async response(request: Request): Promise<Response> {
        if (request.method !== "GET") {
            return new Response("Method Not Allowed", { status: 405 });
        }
        try {
            const path = await this.resolveAuthorizedFile(request.url);
            const contentType =
                mime.contentType(mime.lookup(path) || "application/octet-stream") ||
                "application/octet-stream";
            /* SAFETY: Node read streams are transformed into a web body by `Readable.toWeb`
             * for Electron fetch responses in this environment. */
            const body = Readable.toWeb(createReadStream(path)) as BodyInit;
            return new Response(body, { headers: { "content-type": contentType }, status: 200 });
        } catch {
            return new Response("Not Found", { status: 404 });
        }
    }
}

const MANAGER_SCHEME_PRIVILEGES: PrivilegedScheme[] = [
    {
        privileges: {
            secure: true,
            standard: true,
            supportFetchAPI: true,
        },
        scheme: "vosulay",
    },
    {
        privileges: {
            secure: true,
            standard: true,
            stream: true,
            supportFetchAPI: true,
        },
        scheme: "vosulay-overlay",
    },
];

let schemesRegistered = false;
export function registerCustomSchemes(): void {
    if (schemesRegistered) {
        return;
    }
    electronProtocol().registerSchemesAsPrivileged(MANAGER_SCHEME_PRIVILEGES);
    schemesRegistered = true;
}

export function installCustomProtocols(
    options: RuntimeOptions,
    registry: LocalOverlayRegistry,
): void {
    const protocol = electronProtocol();
    protocol.handle("vosulay", (request) => {
        const url = new URL(request.url);
        if (request.method !== "GET" || url.href !== "vosulay://manager/index.html") {
            return new Response("Not Found", { status: 404 });
        }
        return new Response(buildManagerDocument(options.fpsLimit, options.vosuUrl.origin), {
            headers: {
                "content-type": "text/html; charset=utf-8",
                "permissions-policy": "webgpu=*",
            },
        });
    });
    protocol.handle("vosulay-overlay", (request) => registry.response(request));
}
