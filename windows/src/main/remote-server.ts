import { once } from "node:events";
import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { isIP } from "node:net";
import type { AddressInfo } from "node:net";
import mime from "mime-types";

import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import type { AppConfig, Placement, Profile } from "./types";
import { counterUrl } from "./urls";

const DEFAULT_HOST = "0.0.0.0";
const DEFAULT_PORT = 5050;
const TOKEN_PATTERN = /^[0-9a-f]{32}$/;
const ROOT_PATTERN = /^[A-Za-z0-9_-]+$/;
const OVERLAY_CSP = "default-src 'none'; frame-src 'self' http: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors *";

export interface RemoteOverlayServerOptions {
    runtime: RuntimeOptions;
    registry: LocalOverlayRegistry;
    getConfig(): Promise<AppConfig>;
    host?: string;
    port?: number;
}

function ipv4Parts(value: string): number[] | null {
    if (isIP(value) !== 4) return null;
    return value.split(".").map(Number);
}

function ipv6Parts(value: string): number[] | null {
    if (isIP(value) !== 6 || value.includes("%")) return null;
    let source = value.toLowerCase();
    const dotted = source.lastIndexOf(":");
    if (source.includes(".") && dotted >= 0) {
        const ipv4 = ipv4Parts(source.slice(dotted + 1));
        if (ipv4 === null) return null;
        source = `${source.slice(0, dotted)}:${((ipv4[0] ?? 0) << 8 | (ipv4[1] ?? 0)).toString(16)}:${((ipv4[2] ?? 0) << 8 | (ipv4[3] ?? 0)).toString(16)}`;
    }
    const halves = source.split("::");
    if (halves.length > 2) return null;
    const left = halves[0] ? halves[0].split(":") : [];
    const right = halves[1] ? halves[1].split(":") : [];
    const missing = 8 - left.length - right.length;
    if ((halves.length === 1 && missing !== 0) || (halves.length === 2 && missing < 1)) return null;
    const words = [...left, ...Array.from({ length: missing }, () => "0"), ...right].map((part) => Number.parseInt(part, 16));
    return words.length === 8 && words.every((part) => Number.isInteger(part) && part >= 0 && part <= 0xffff) ? words : null;
}

function normalizeIp(value: string): string | null {
    const version = isIP(value);
    if (version === 4) return value;
    if (version !== 6) return null;
    const parts = ipv6Parts(value);
    if (parts === null) return null;
    if (parts.slice(0, 5).every((part) => part === 0) && parts[5] === 0xffff) {
        return `${(parts[6] ?? 0) >> 8}.${(parts[6] ?? 0) & 0xff}.${(parts[7] ?? 0) >> 8}.${(parts[7] ?? 0) & 0xff}`;
    }
    return value;
}

function sameIp(left: string, right: string): boolean {
    const normalizedLeft = normalizeIp(left);
    const normalizedRight = normalizeIp(right);
    if (normalizedLeft === null || normalizedRight === null) return false;
    const version = isIP(normalizedLeft);
    if (version !== isIP(normalizedRight)) return false;
    const leftParts = version === 4 ? ipv4Parts(normalizedLeft) : ipv6Parts(normalizedLeft);
    const rightParts = version === 4 ? ipv4Parts(normalizedRight) : ipv6Parts(normalizedRight);
    return leftParts !== null && rightParts !== null && leftParts.every((part, index) => part === rightParts[index]);
}

function isLoopback(address: string): boolean {
    const normalized = normalizeIp(address);
    if (normalized === null) return false;
    const ipv4 = ipv4Parts(normalized);
    if (ipv4 !== null) return ipv4[0] === 127;
    const ipv6 = ipv6Parts(normalized);
    return ipv6 !== null && ipv6.slice(0, 7).every((part) => part === 0) && ipv6[7] === 1;
}

function matchesIpv4Pattern(address: string, pattern: string): boolean {
    const normalized = normalizeIp(address);
    const addressParts = normalized === null ? null : ipv4Parts(normalized);
    const patternParts = pattern.split(".");
    if (addressParts === null || patternParts.length !== 4) return false;
    return patternParts.every((part, index) => {
        if (part === "*") return true;
        if (!/^\d+$/.test(part)) return false;
        const octet = Number(part);
        return octet >= 0 && octet <= 255 && String(octet) === part && octet === addressParts[index];
    });
}

export function isPeerAllowed(remoteAddress: string | undefined, allowedIps: string): boolean {
    if (remoteAddress === undefined || normalizeIp(remoteAddress) === null) return false;
    if (isLoopback(remoteAddress)) return true;
    for (const token of allowedIps.split(",").map((value) => value.trim()).filter(Boolean)) {
        if (sameIp(remoteAddress, token) || matchesIpv4Pattern(remoteAddress, token)) return true;
    }
    return false;
}

function htmlEscape(value: string): string {
    return value.replace(/[&<>"']/g, (character) => ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        "\"": "&quot;",
        "'": "&#39;",
    })[character] ?? character);
}

function localCapability(value: string): string | null {
    try {
        const url = new URL(value);
        const parts = url.pathname.split("/");
        if (url.protocol.toLowerCase() !== "vosulay-overlay:" || !TOKEN_PATTERN.test(url.hostname) || url.port || url.username || url.password) return null;
        if (parts.length < 3 || parts[0] !== "" || !parts[1] || !ROOT_PATTERN.test(parts[1]) || parts[1].includes("=")) return null;
        return `${url.hostname}/${parts[1]}`;
    } catch {
        return null;
    }
}

function remoteLocalUrl(value: string): string | null {
    const capability = localCapability(value);
    if (capability === null) return null;
    const url = new URL(value);
    return `/local/${url.hostname}${url.pathname}${url.search}${url.hash}`;
}

function vosuUrlForRequest(baseUrl: URL, requestHost: string | undefined): URL {
    const hostname = baseUrl.hostname.startsWith("[") && baseUrl.hostname.endsWith("]") ? baseUrl.hostname.slice(1, -1) : baseUrl.hostname;
    if (requestHost === undefined || (hostname.toLowerCase() !== "localhost" && !isLoopback(hostname))) return baseUrl;
    try {
        const requestUrl = new URL(`http://${requestHost}/`);
        if (requestUrl.username !== "" || requestUrl.password !== "" || requestUrl.pathname !== "/" || requestUrl.search !== "" || requestUrl.hash !== "") {
            return baseUrl;
        }
        const result = new URL(baseUrl);
        result.hostname = requestUrl.hostname;
        return result;
    } catch {
        return baseUrl;
    }
}

interface PlacementSource {
    source: string;
    custom: boolean;
}

function placementSource(placement: Placement, vosuUrl: URL): PlacementSource | null {
    if (placement.url !== undefined) {
        const local = remoteLocalUrl(placement.url);
        if (local !== null) return { source: local, custom: true };
        try {
            const custom = new URL(placement.url);
            if (custom.protocol === "http:" || custom.protocol === "https:") {
                return { source: custom.toString(), custom: true };
            }
        } catch {
            // An invalid custom source is unavailable, not a counter placement.
        }
        return null;
    }
    return { source: counterUrl(vosuUrl, placement.counter_id).toString(), custom: false };
}

function renderPlacement(placement: Placement, vosuUrl: URL): string | null {
    const resolved = placementSource(placement, vosuUrl);
    if (resolved === null) return null;
    const title = placement.name?.trim() || placement.counter_id || "Overlay";
    const style = `position:absolute;left:${placement.left}px;top:${placement.top}px;width:${placement.width}px;height:${placement.height}px;transform-origin:0 0;transform:scale(${placement.scale}) rotate(${placement.rotation}deg);opacity:${placement.opacity};z-index:${placement.z_index};border:0;background:transparent`;
    const sandbox = resolved.custom ? ` sandbox="allow-scripts allow-same-origin"` : "";
    return `<iframe src="${htmlEscape(resolved.source)}" title="${htmlEscape(title)}" allow="autoplay" loading="eager"${sandbox} referrerpolicy="no-referrer" style="${style}"></iframe>`;
}

export function renderProfileDocument(profile: Profile, runtime: RuntimeOptions, requestHost?: string): string {
    const vosuUrl = vosuUrlForRequest(runtime.vosuUrl, requestHost);
    const frames = profile.overlays.map((placement) => renderPlacement(placement, vosuUrl)).filter((frame): frame is string => frame !== null).join("\n");
    return `<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${htmlEscape(profile.name)}</title><style>*{box-sizing:border-box}html,body,#stage{width:100%;height:100%;margin:0;overflow:hidden;background:transparent}#stage{position:fixed;inset:0}</style></head><body><main id="stage">${frames}</main></body></html>`;
}

class ProfileSelectionError extends Error {
    constructor(readonly status: 400 | 404, message: string) {
        super(message);
    }
}

function selectProfile(url: URL, config: AppConfig): Profile | null | ProfileSelectionError {
    const rawQuery = url.search.startsWith("?") ? url.search.slice(1) : "";
    if (rawQuery === "") return config.profiles.find((profile) => profile.id === config.active_profile) ?? null;
    const parameters = rawQuery.split("&");
    if (parameters.length !== 1) return new ProfileSelectionError(400, "profile must be specified exactly once");
    const separator = parameters[0]?.indexOf("=") ?? -1;
    if (separator < 0) return new ProfileSelectionError(400, "malformed profile query");
    let name: string;
    let value: string;
    try {
        const decode = (part: string): string => decodeURIComponent(part.replace(/\+/g, " "));
        name = decode(parameters[0]?.slice(0, separator) ?? "");
        value = decode(parameters[0]?.slice(separator + 1) ?? "");
    } catch {
        return new ProfileSelectionError(400, "malformed profile query");
    }
    if (name !== "profile") return new ProfileSelectionError(400, "profile must be the only query parameter");
    if (!/^[1-9]\d*$/.test(value)) return new ProfileSelectionError(400, "profile must be a positive 1-based integer");
    const index = Number(value);
    if (!Number.isSafeInteger(index)) return new ProfileSelectionError(400, "profile index is too large");
    if (index > config.profiles.length) return new ProfileSelectionError(404, `profile index ${index} is out of range`);
    return config.profiles[index - 1] ?? null;
}

function sendText(response: ServerResponse, status: number, body: string, headers: Record<string, string> = {}): void {
    const content = Buffer.from(body, "utf8");
    response.writeHead(status, {
        "cache-control": "no-store",
        "content-type": "text/plain; charset=utf-8",
        "content-length": String(content.byteLength),
        "x-content-type-options": "nosniff",
        ...headers,
    });
    response.end(content);
}

export class RemoteOverlayServer {
    private readonly server: Server;
    private readonly options: RemoteOverlayServerOptions;

    constructor(options: RemoteOverlayServerOptions) {
        this.options = options;
        this.server = createServer((request, response) => {
            void this.handle(request, response).catch(() => {
                if (!response.headersSent) sendText(response, 500, "Internal Server Error\n");
                else response.destroy();
            });
        });
    }

    async start(): Promise<void> {
        if (this.server.listening) return;
        const listening = once(this.server, "listening");
        this.server.listen(this.options.port ?? DEFAULT_PORT, this.options.host ?? DEFAULT_HOST);
        await listening;
    }

    async stop(): Promise<void> {
        if (!this.server.listening) return;
        const closed = once(this.server, "close");
        this.server.close();
        this.server.closeIdleConnections();
        await closed;
    }

    address(): AddressInfo | null {
        const address = this.server.address();
        return address !== null && typeof address !== "string" ? address : null;
    }

    private async handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
        const config = (await this.options.getConfig());
        if (!isPeerAllowed(request.socket.remoteAddress, config.settings.allowed_ips)) {
            sendText(response, 403, "Forbidden\n");
            return;
        }
        const url = new URL(request.url ?? "/", "http://localhost");
        if (url.pathname !== "/overlay" && !url.pathname.startsWith("/local/")) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        if (request.method !== "GET" && request.method !== "HEAD") {
            sendText(response, 405, "Method Not Allowed\n", { allow: "GET, HEAD" });
            return;
        }
        if (url.pathname === "/overlay") {
            this.serveOverlay(request, response, url, config);
            return;
        }
        await this.serveLocal(request, response, url, config);
    }

    private serveOverlay(request: IncomingMessage, response: ServerResponse, url: URL, config: AppConfig): void {
        const selected = selectProfile(url, config);
        if (selected instanceof ProfileSelectionError) {
            sendText(response, selected.status, `${selected.message}\n`);
            return;
        }
        if (selected === null) {
            sendText(response, 404, "Profile not found\n");
            return;
        }
        const content = Buffer.from(renderProfileDocument(selected, this.options.runtime, request.headers.host), "utf8");
        response.writeHead(200, {
            "cache-control": "no-store",
            "content-type": "text/html; charset=utf-8",
            "content-length": String(content.byteLength),
            "content-security-policy": OVERLAY_CSP,
            "referrer-policy": "no-referrer",
            "x-content-type-options": "nosniff",
            "cross-origin-resource-policy": "cross-origin",
        });
        response.end(request.method === "HEAD" ? undefined : content);
    }

    private async serveLocal(request: IncomingMessage, response: ServerResponse, url: URL, config: AppConfig): Promise<void> {
        const raw = url.pathname.slice("/local/".length);
        const slash = raw.indexOf("/");
        const rootSlash = slash < 0 ? -1 : raw.indexOf("/", slash + 1);
        if (slash < 0 || rootSlash < 0) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const capability = raw.slice(0, rootSlash);
        let configuredUrl: string | null = null;
        for (const profile of config.profiles) {
            for (const placement of profile.overlays) {
                if (placement.url && localCapability(placement.url) === capability) {
                    configuredUrl = placement.url;
                    break;
                }
            }
            if (configuredUrl !== null) break;
        }
        if (configuredUrl === null || !this.options.registry.authorizeUrl(configuredUrl)) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const localUrl = `vosulay-overlay://${raw}${url.search}`;
        let path: string;
        try {
            path = await this.options.registry.resolveAuthorizedFile(localUrl);
        } catch {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const info = await stat(path);
        const contentType = mime.contentType(mime.lookup(path) || "application/octet-stream") || "application/octet-stream";
        response.writeHead(200, {
            "cache-control": "no-store",
            "content-type": contentType,
            "content-length": String(info.size),
            "referrer-policy": "no-referrer",
            "x-content-type-options": "nosniff",
            "cross-origin-resource-policy": "cross-origin",
        });
        if (request.method === "HEAD") {
            response.end();
            return;
        }
        const stream = createReadStream(path);
        stream.on("error", () => response.destroy());
        stream.pipe(response);
    }
}
