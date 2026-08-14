import { createReadStream, realpathSync, statSync } from "node:fs";
import { access, realpath, stat } from "node:fs/promises";
import { basename, dirname, isAbsolute, relative, sep } from "node:path";
import { Readable } from "node:stream";
import { createRequire } from "node:module";
import { randomBytes } from "node:crypto";
import mime from "mime-types";
import type { RuntimeOptions } from "./options";
import { buildManagerDocument } from "./manager-document";

const require = createRequire(import.meta.url);
const TOKEN_PATTERN = /^[0-9a-f]{32}$/;
const ROOT_PATTERN = /^[A-Za-z0-9_-]+$/;

interface ElectronProtocol {
    registerSchemesAsPrivileged(schemes: unknown[]): void;
    handle(scheme: string, handler: (request: Request) => Promise<Response> | Response): void;
}

function electronProtocol(): ElectronProtocol {
    return (require("electron") as { protocol: ElectronProtocol }).protocol;
}

interface ParsedOverlayUrl {
    token: string;
    root: string;
    segments: string[];
}

function decodeRoot(segment: string): string {
    if (!ROOT_PATTERN.test(segment) || segment.includes("=")) throw new Error("invalid local overlay root encoding");
    const bytes = Buffer.from(segment, "base64url");
    if (bytes.toString("base64url") !== segment) throw new Error("non-canonical local overlay root encoding");
    const root = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (!root || !isAbsolute(root) || Buffer.from(root, "utf8").toString("base64url") !== segment) {
        throw new Error("invalid local overlay root");
    }
    try {
        if (realpathSync.native(root) !== root || !statSync(root).isDirectory()) throw new Error("non-canonical local overlay root");
    } catch {
        throw new Error("local overlay root is not a canonical readable directory");
    }
    return root;
}

function parseOverlayUrl(value: URL | string): ParsedOverlayUrl {
    const url = value instanceof URL ? value : new URL(value);
    if (url.protocol.toLowerCase() !== "vosulay-overlay:" || !TOKEN_PATTERN.test(url.hostname) || url.port || url.username || url.password) {
        throw new Error("invalid local overlay URL");
    }
    const encoded = url.pathname.split("/");
    if (encoded.length < 3 || encoded[0] !== "" || !encoded[1]) throw new Error("invalid local overlay path");
    const segments = encoded.slice(2).map((segment) => {
        let decoded: string;
        try {
            decoded = decodeURIComponent(segment);
        } catch {
            throw new Error("invalid local overlay path encoding");
        }
        if (!decoded || decoded === "." || decoded === ".." || decoded.includes("/") || decoded.includes("\\") || decoded.includes("\0")) {
            throw new Error("unsafe local overlay path");
        }
        return decoded;
    });
    return { token: url.hostname, root: decodeRoot(encoded[1]), segments };
}

function confined(root: string, target: string): boolean {
    const path = relative(root, target);
    return path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path);
}

export class LocalOverlayRegistry {
    private readonly roots = new Map<string, string>();

    async createUrl(filePath: string): Promise<string> {
        const file = await realpath(filePath);
        const info = await stat(file);
        await access(file);
        if (!info.isFile()) throw new Error("selected overlay is not a regular file");
        const root = await realpath(dirname(file));
        const token = randomBytes(16).toString("hex");
        const url = `vosulay-overlay://${token}/${Buffer.from(root, "utf8").toString("base64url")}/${encodeURIComponent(basename(file))}`;
        if (!this.authorizeUrl(url)) throw new Error("selected overlay could not be authorized safely");
        return url;
    }

    authorizeUrl(value: URL | string): boolean {
        try {
            const parsed = parseOverlayUrl(value);
            const existing = this.roots.get(parsed.token);
            if (existing !== undefined && existing !== parsed.root) return false;
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
                const existing = this.roots.get(parsed.token) ?? additions.get(parsed.token);
                if (existing !== undefined && existing !== parsed.root) return false;
                additions.set(parsed.token, parsed.root);
            }
        } catch {
            return false;
        }
        for (const [token, root] of additions) this.roots.set(token, root);
        return true;
    }

    async resolveAuthorizedFile(value: URL | string): Promise<string> {
        const parsed = parseOverlayUrl(value);
        const authorized = this.roots.get(parsed.token);
        if (authorized === undefined || authorized !== parsed.root) throw new Error("local overlay URL is not authorized");
        const canonicalRoot = await realpath(parsed.root);
        if (canonicalRoot !== parsed.root || canonicalRoot !== authorized) throw new Error("local overlay root is no longer canonical");
        const target = await realpath([canonicalRoot, ...parsed.segments].join(sep));
        if (!confined(canonicalRoot, target)) throw new Error("local overlay path escapes its root");
        const info = await stat(target);
        if (!info.isFile()) throw new Error("local overlay target is not a regular file");
        await access(target);
        return target;
    }

    async response(request: Request): Promise<Response> {
        if (request.method !== "GET") return new Response("Method Not Allowed", { status: 405 });
        try {
            const path = await this.resolveAuthorizedFile(request.url);
            const contentType = mime.contentType(mime.lookup(path) || "application/octet-stream") || "application/octet-stream";
            const body = Readable.toWeb(createReadStream(path)) as unknown as BodyInit;
            return new Response(body, { status: 200, headers: { "content-type": contentType } });
        } catch {
            return new Response("Not Found", { status: 404 });
        }
    }
}

let schemesRegistered = false;
export function registerCustomSchemes(): void {
    if (schemesRegistered) return;
    electronProtocol().registerSchemesAsPrivileged([
        { scheme: "vosulay", privileges: { standard: true, supportFetchAPI: true } },
        { scheme: "vosulay-overlay", privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } },
    ]);
    schemesRegistered = true;
}

export function installCustomProtocols(options: RuntimeOptions, registry: LocalOverlayRegistry): void {
    const protocol = electronProtocol();
    protocol.handle("vosulay", (request) => {
        const url = new URL(request.url);
        if (request.method !== "GET" || url.href !== "vosulay://manager/index.html") return new Response("Not Found", { status: 404 });
        return new Response(buildManagerDocument(options.fpsLimit), { headers: { "content-type": "text/html; charset=utf-8" } });
    });
    protocol.handle("vosulay-overlay", (request) => registry.response(request));
}
