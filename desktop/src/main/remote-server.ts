import { timingSafeEqual } from "node:crypto";
import { once } from "node:events";
import { createReadStream } from "node:fs";
import { pipeline } from "node:stream/promises";
import { stat } from "node:fs/promises";
import { type IncomingMessage, type Server, type ServerResponse, createServer } from "node:http";
import { isIP } from "node:net";
import type { AddressInfo } from "node:net";
import mime from "mime-types";

import type { LocalOverlayRegistry } from "./local-overlays";
import type { RuntimeOptions } from "./options";
import type { AppConfig, Placement, Profile } from "./types";
import { counterUrl } from "./urls";

const DEFAULT_HOST = "0.0.0.0",
    DEFAULT_PORT = 5050,
    MAX_ACTIVE_REQUESTS = 32,
    MAX_REQUEST_HEADER_BYTES = 16 * 1024,
    ACCESS_TOKEN_PATTERN = /^[0-9a-f]{64}$/,
    OVERLAY_CSP =
        "default-src 'none'; frame-src 'self' http: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors *",
    OVERLAY_PERMISSIONS_POLICY = "webgpu=*",
    REQUEST_TIMEOUT_MS = 3000,
    ROOT_PATTERN = /^[A-Za-z0-9_-]+$/,
    TOKEN_PATTERN = /^[0-9a-f]{32}$/;

export interface RemoteOverlayServerOptions {
    runtime: RuntimeOptions;
    registry: LocalOverlayRegistry;
    initialConfig: AppConfig;
    host?: string;
    port?: number;
}

function ipv4Parts(value: string): number[] | undefined {
    if (isIP(value) !== 4) {
        return undefined;
    }
    return value.split(".").map(Number);
}

function ipv6Parts(value: string): number[] | undefined {
    if (isIP(value) !== 6 || value.includes("%")) {
        return undefined;
    }
    let source = value.toLowerCase();
    const dotted = source.lastIndexOf(":");
    if (source.includes(".") && dotted !== -1) {
        const ipv4 = ipv4Parts(source.slice(dotted + 1));
        if (ipv4 === undefined) {
            return undefined;
        }
        source = `${source.slice(0, dotted)}:${(((ipv4[0] ?? 0) << 8) | (ipv4[1] ?? 0)).toString(16)}:${(((ipv4[2] ?? 0) << 8) | (ipv4[3] ?? 0)).toString(16)}`;
    }
    const halves = source.split("::");
    if (halves.length > 2) {
        return undefined;
    }
    const left = halves[0] ? halves[0].split(":") : [],
        right = halves[1] ? halves[1].split(":") : [],
        missing = 8 - left.length - right.length;
    if ((halves.length === 1 && missing !== 0) || (halves.length === 2 && missing < 1)) {
        return undefined;
    }
    const words = [...left, ...Array.from({ length: missing }, () => "0"), ...right].map((part) =>
        Number.parseInt(part, 16),
    );
    return words.length === 8 &&
        words.every((part) => Number.isInteger(part) && part >= 0 && part <= 0xFF_FF)
        ? words
        : undefined;
}

function normalizeIp(value: string): string | undefined {
    const version = isIP(value);
    if (version === 4) {
        return value;
    }
    if (version !== 6) {
        return undefined;
    }
    const parts = ipv6Parts(value);
    if (parts === undefined) {
        return undefined;
    }
    if (parts.slice(0, 5).every((part) => part === 0) && parts[5] === 0xFF_FF) {
        return `${(parts[6] ?? 0) >> 8}.${(parts[6] ?? 0) & 0xFF}.${(parts[7] ?? 0) >> 8}.${(parts[7] ?? 0) & 0xFF}`;
    }
    return value;
}

function sameIp(left: string, right: string): boolean {
    const normalizedLeft = normalizeIp(left),
        normalizedRight = normalizeIp(right);
    if (normalizedLeft === undefined || normalizedRight === undefined) {
        return false;
    }
    const version = isIP(normalizedLeft);
    if (version !== isIP(normalizedRight)) {
        return false;
    }
    const leftParts = version === 4 ? ipv4Parts(normalizedLeft) : ipv6Parts(normalizedLeft),
        rightParts = version === 4 ? ipv4Parts(normalizedRight) : ipv6Parts(normalizedRight);
    return (
        leftParts !== undefined &&
        rightParts !== undefined &&
        leftParts.every((part, index) => part === rightParts[index])
    );
}

function isLoopback(address: string): boolean {
    const normalized = normalizeIp(address);
    if (normalized === undefined) {
        return false;
    }
    const ipv4 = ipv4Parts(normalized);
    if (ipv4 !== undefined) {
        return ipv4[0] === 127;
    }
    const ipv6 = ipv6Parts(normalized);
    return ipv6 !== undefined && ipv6.slice(0, 7).every((part) => part === 0) && ipv6[7] === 1;
}

function matchesIpv4Pattern(address: string, pattern: string): boolean {
    const normalized = normalizeIp(address),
        addressParts = normalized === undefined ? undefined : ipv4Parts(normalized),
        patternParts = pattern.split(".");
    if (addressParts === undefined || patternParts.length !== 4) {
        return false;
    }
    return patternParts.every((part, index) => {
        if (part === "*") {
            return true;
        }
        if (!/^\d+$/.test(part)) {
            return false;
        }
        const octet = Number(part);
        return (
            octet >= 0 && octet <= 255 && String(octet) === part && octet === addressParts[index]
        );
    });
}

export function isPeerAllowed(remoteAddress: string | undefined, allowedIps: string): boolean {
    if (remoteAddress === undefined || normalizeIp(remoteAddress) === undefined) {
        return false;
    }
    if (isLoopback(remoteAddress)) {
        return true;
    }
    for (const token of allowedIps
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean)) {
        if (sameIp(remoteAddress, token) || matchesIpv4Pattern(remoteAddress, token)) {
            return true;
        }
    }
    return false;
}

function htmlEscape(value: string): string {
    return value.replaceAll(
        /[&<>"']/g,
        (character) =>
            ({
                '"': "&quot;",
                "&": "&amp;",
                "'": "&#39;",
                "<": "&lt;",
                ">": "&gt;",
            })[character] ?? character,
    );
}

function localCapability(value: string): string | undefined {
    try {
        const url = new URL(value),
            parts = url.pathname.split("/");
        if (
            url.protocol.toLowerCase() !== "vosulay-overlay:" ||
            !TOKEN_PATTERN.test(url.hostname) ||
            url.port ||
            url.username ||
            url.password
        ) {
            return undefined;
        }
        if (
            parts.length < 3 ||
            parts[0] !== "" ||
            !parts[1] ||
            !ROOT_PATTERN.test(parts[1]) ||
            parts[1].includes("=")
        ) {
            return undefined;
        }
        return `${url.hostname}/${parts[1]}`;
    } catch {
        return undefined;
    }
}

function remoteLocalUrl(value: string, credential: string): string | undefined {
    const capability = localCapability(value);
    if (capability === undefined) {
        return undefined;
    }
    const url = new URL(value),
        [, , ...relativeSegments] = url.pathname.split("/");
    if (relativeSegments.length === 0) {
        return undefined;
    }
    return `/access/${credential}/local/${url.hostname}/${relativeSegments.join("/")}${url.search}${url.hash}`;
}

function vosuUrlForRequest(baseUrl: URL, requestHost: string | undefined): URL {
    const hostname =
        baseUrl.hostname.startsWith("[") && baseUrl.hostname.endsWith("]")
            ? baseUrl.hostname.slice(1, -1)
            : baseUrl.hostname;
    if (
        requestHost === undefined ||
        (hostname.toLowerCase() !== "localhost" && !isLoopback(hostname))
    ) {
        return baseUrl;
    }
    try {
        const requestUrl = new URL(`http://${requestHost}/`);
        if (
            requestUrl.username !== "" ||
            requestUrl.password !== "" ||
            requestUrl.pathname !== "/" ||
            requestUrl.search !== "" ||
            requestUrl.hash !== ""
        ) {
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
    kind: "counter" | "local" | "remote";
}

function placementSource(
    placement: Placement,
    vosuUrl: URL,
    credential: string,
): PlacementSource | undefined {
    if (placement.url !== undefined) {
        const local = remoteLocalUrl(placement.url, credential);
        if (local !== undefined) {
            return { kind: "local", source: local };
        }
        try {
            const custom = new URL(placement.url);
            if (custom.protocol === "http:" || custom.protocol === "https:") {
                return { kind: "remote", source: custom.toString() };
            }
        } catch {
            // An invalid custom source is unavailable, not a counter placement.
        }
        return undefined;
    }
    return { kind: "counter", source: counterUrl(vosuUrl, placement.counter_id).toString() };
}

function renderPlacement(placement: Placement, vosuUrl: URL, credential: string): string | undefined {
    const resolved = placementSource(placement, vosuUrl, credential);
    if (resolved === undefined) {
        return undefined;
    }
    const title = placement.name?.trim() || placement.counter_id || "Overlay",
        style = `position:absolute;left:${placement.left}px;top:${placement.top}px;width:${placement.width}px;height:${placement.height}px;transform-origin:0 0;transform:scale(${placement.scale}) rotate(${placement.rotation}deg);opacity:${placement.opacity};z-index:${placement.z_index};border:0;background:transparent`,
        sandbox =
            resolved.kind === "local"
                ? ` sandbox="allow-scripts"`
                : (resolved.kind === "remote"
                  ? ` sandbox="allow-scripts allow-same-origin"`
                  : ""),
        // Arbitrary remote URLs are untrusted content. They retain ordinary media
        // Access but do not receive GPU delegation by default. Installed counters
        // And HMAC-authenticated local overlays are the explicitly trusted sources.
        allow = resolved.kind === "remote" ? "autoplay" : "autoplay; webgpu";
    return `<iframe src="${htmlEscape(resolved.source)}" title="${htmlEscape(title)}" allow="${allow}" loading="eager"${sandbox} referrerpolicy="no-referrer" style="${style}"></iframe>`;
}

export function renderProfileDocument(
    profile: Profile,
    runtime: RuntimeOptions,
    credential: string,
    requestHost?: string,
): string {
    const vosuUrl = vosuUrlForRequest(runtime.vosuUrl, requestHost),
        frames = profile.overlays
            .map((placement) => renderPlacement(placement, vosuUrl, credential))
            .filter((frame): frame is string => frame !== undefined)
            .join("\n");
    return `<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${htmlEscape(profile.name)}</title><style>*{box-sizing:border-box}html,body,#stage{width:100%;height:100%;margin:0;overflow:hidden;background:transparent}#stage{position:fixed;inset:0}</style></head><body><main id="stage">${frames}</main></body></html>`;
}

class ProfileSelectionError extends Error {
    constructor(
        readonly status: 400 | 404,
        message: string,
    ) {
        super(message);
    }
}

function selectProfile(url: URL, config: AppConfig): Profile | undefined | ProfileSelectionError {
    const rawQuery = url.search.startsWith("?") ? url.search.slice(1) : "";
    if (rawQuery === "") {
        return config.profiles.find((profile) => profile.id === config.active_profile) ?? undefined;
    }
    const parameters = rawQuery.split("&");
    if (parameters.length !== 1) {
        return new ProfileSelectionError(400, "profile must be specified exactly once");
    }
    const separator = parameters[0]?.indexOf("=") ?? -1;
    if (separator < 0) {
        return new ProfileSelectionError(400, "malformed profile query");
    }
    let name: string, value: string;
    try {
        const decode = (part: string): string => decodeURIComponent(part.replaceAll("+", " "));
        name = decode(parameters[0]?.slice(0, separator) ?? "");
        value = decode(parameters[0]?.slice(separator + 1) ?? "");
    } catch {
        return new ProfileSelectionError(400, "malformed profile query");
    }
    if (name !== "profile") {
        return new ProfileSelectionError(400, "profile must be the only query parameter");
    }
    if (!/^[1-9]\d*$/.test(value)) {
        return new ProfileSelectionError(400, "profile must be a positive 1-based integer");
    }
    const index = Number(value);
    if (!Number.isSafeInteger(index)) {
        return new ProfileSelectionError(400, "profile index is too large");
    }
    if (index > config.profiles.length) {
        return new ProfileSelectionError(404, `profile index ${index} is out of range`);
    }
    return config.profiles[index - 1] ?? undefined;
}

function sendText(
    response: ServerResponse,
    status: number,
    body: string,
    headers: Record<string, string> = {},
): void {
    const content = Buffer.from(body, "utf8");
    response.writeHead(status, {
        "cache-control": "no-store",
        "content-length": String(content.byteLength),
        "content-type": "text/plain; charset=utf-8",
        "x-content-type-options": "nosniff",
        ...headers,
    });
    response.end(content);
}

interface ProtectedRoute {
    credential: string;
    path: string;
}

function protectedRoute(pathname: string): ProtectedRoute | undefined {
    const protectedPath = pathname.startsWith("/access/") ? pathname.slice("/access/".length) : "",
        separator = protectedPath.indexOf("/");
    if (separator < 1) {
        return undefined;
    }
    const credential = protectedPath.slice(0, separator),
        path = protectedPath.slice(separator + 1);
    return path === "overlay" || path.startsWith("local/") ? { credential, path } : undefined;
}

function credentialMatches(provided: string, expected: string): boolean {
    if (!ACCESS_TOKEN_PATTERN.test(provided) || !ACCESS_TOKEN_PATTERN.test(expected)) {
        return false;
    }
    return timingSafeEqual(Buffer.from(provided, "ascii"), Buffer.from(expected, "ascii"));
}

// A header present more than once is malformed and must be rejected. This
// Sentinel distinguishes that from an absent header, which returns undefined.
const MALFORMED_HEADER = Symbol("malformed-header");

function singleHeader(
    request: IncomingMessage,
    name: string,
): string | undefined | typeof MALFORMED_HEADER {
    const values: string[] = [];
    for (let index = 0; index < request.rawHeaders.length; index += 2) {
        if (request.rawHeaders[index]?.toLowerCase() === name) {
            values.push(request.rawHeaders[index + 1] ?? "");
        }
    }
    if (values.length > 1) {
        return MALFORMED_HEADER;
    }
    return values[0];
}

function requestAuthority(
    host: string,
    request: IncomingMessage,
    configuredHost: string,
): URL | undefined {
    const { localAddress } = request.socket,
        { localPort } = request.socket;
    if (localAddress === undefined || localPort === undefined) {
        return undefined;
    }
    try {
        const url = new URL(`http://${host}/`),
            hostname =
                url.hostname.startsWith("[") && url.hostname.endsWith("]")
                    ? url.hostname.slice(1, -1)
                    : url.hostname;
        if (
            url.username ||
            url.password ||
            url.pathname !== "/" ||
            url.search ||
            url.hash ||
            !url.port ||
            Number(url.port) !== localPort
        ) {
            return undefined;
        }
        if (hostname.toLowerCase() === "localhost") {
            return isLoopback(localAddress) ? url : undefined;
        }
        if (configuredHost && hostname.toLowerCase() === configuredHost.toLowerCase()) {
            return url;
        }
        return sameIp(hostname, localAddress) ? url : undefined;
    } catch {
        return undefined;
    }
}

function requestMetadataIsAllowed(
    request: IncomingMessage,
    configuredHost: string,
    allowOpaqueOrigin: boolean,
): boolean {
    const host = singleHeader(request, "host");
    if (typeof host !== "string" || !host) {
        return false;
    }
    const authority = requestAuthority(host, request, configuredHost);
    if (authority === undefined) {
        return false;
    }
    const fetchSite = singleHeader(request, "sec-fetch-site");
    if (fetchSite === MALFORMED_HEADER) {
        return false;
    }
    const fetchSiteAllowed =
        fetchSite === undefined ||
        ["none", "same-origin", "same-site"].includes(fetchSite) ||
        (allowOpaqueOrigin && fetchSite === "cross-site");
    if (!fetchSiteAllowed) {
        return false;
    }
    const origin = singleHeader(request, "origin");
    if (origin === MALFORMED_HEADER) {
        return false;
    }
    if (origin === undefined || (allowOpaqueOrigin && origin === "null")) {
        return true;
    }
    try {
        const url = new URL(origin);
        return (
            url.protocol === "http:" &&
            !url.username &&
            !url.password &&
            url.pathname === "/" &&
            !url.search &&
            !url.hash &&
            url.origin === authority.origin
        );
    } catch {
        return false;
    }
}

export class RemoteOverlayServer {
    private readonly server: Server;
    private readonly options: RemoteOverlayServerOptions;
    private config: AppConfig;
    private activeRequests = 0;

    constructor(options: RemoteOverlayServerOptions) {
        this.options = options;
        this.config = options.initialConfig;
        this.server = createServer(
            {
                headersTimeout: REQUEST_TIMEOUT_MS,
                keepAliveTimeout: REQUEST_TIMEOUT_MS,
                maxHeaderSize: MAX_REQUEST_HEADER_BYTES,
                requestTimeout: REQUEST_TIMEOUT_MS,
            },
            (request, response) => {
                if (this.activeRequests >= MAX_ACTIVE_REQUESTS) {
                    sendText(response, 503, "Server is busy\n");
                    return;
                }
                this.activeRequests += 1;
                request.setTimeout(REQUEST_TIMEOUT_MS, () => {
                    if (response.headersSent) {
                        response.destroy();
                    } else {
                        sendText(response, 408, "Request Timeout\n");
                    }
                });
                response.setTimeout(REQUEST_TIMEOUT_MS, () => response.destroy());
                void this.handle(request, response)
                    .catch(() => {
                        if (response.headersSent) {
                            response.destroy();
                        } else {
                            sendText(response, 500, "Internal Server Error\n");
                        }
                    })
                    .finally(() => {
                        this.activeRequests -= 1;
                    });
            },
        );
        this.server.maxConnections = MAX_ACTIVE_REQUESTS * 2;
        this.server.maxRequestsPerSocket = 100;
    }

    updateConfig(config: AppConfig): void {
        this.config = config;
    }

    async start(): Promise<void> {
        if (this.server.listening) {
            return;
        }
        const listening = once(this.server, "listening");
        this.server.listen(this.options.port ?? DEFAULT_PORT, this.options.host ?? DEFAULT_HOST);
        await listening;
    }

    async stop(): Promise<void> {
        if (!this.server.listening) {
            return;
        }
        const closed = once(this.server, "close");
        this.server.close();
        this.server.closeIdleConnections();
        await closed;
    }

    address(): AddressInfo | undefined {
        const address = this.server.address() ?? undefined;
        return address !== undefined && typeof address !== "string" ? address : undefined;
    }

    private async handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
        if (request.method !== "GET" && request.method !== "HEAD") {
            sendText(response, 405, "Method Not Allowed\n", { allow: "GET, HEAD" });
            return;
        }
        const url = new URL(request.url ?? "/", "http://localhost"),
            route = protectedRoute(url.pathname);
        if (route === undefined) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        if (
            !requestMetadataIsAllowed(
                request,
                this.config.settings.remote_access_host,
                route.path.startsWith("local/"),
            ) ||
            !isPeerAllowed(request.socket.remoteAddress, this.config.settings.allowed_ips) ||
            !credentialMatches(route.credential, this.config.settings.remote_access_token)
        ) {
            sendText(response, 403, "Forbidden\n");
            return;
        }
        if (route.path === "overlay") {
            this.serveOverlay(request, response, url, route.credential);
            return;
        }
        await this.serveLocal(request, response, url, route.path);
    }

    private serveOverlay(
        request: IncomingMessage,
        response: ServerResponse,
        url: URL,
        credential: string,
    ): void {
        const selected = selectProfile(url, this.config);
        if (selected instanceof ProfileSelectionError) {
            sendText(response, selected.status, `${selected.message}\n`);
            return;
        }
        if (selected === undefined) {
            sendText(response, 404, "Profile not found\n");
            return;
        }
        const host = singleHeader(request, "host"),
            content = Buffer.from(
                renderProfileDocument(
                    selected,
                    this.options.runtime,
                    credential,
                    typeof host === "string" ? host : undefined,
                ),
                "utf8",
            );
        response.writeHead(200, {
            "cache-control": "no-store",
            "content-length": String(content.byteLength),
            "content-security-policy": OVERLAY_CSP,
            "content-type": "text/html; charset=utf-8",
            "cross-origin-resource-policy": "same-origin",
            "permissions-policy": OVERLAY_PERMISSIONS_POLICY,
            "referrer-policy": "no-referrer",
            "x-content-type-options": "nosniff",
        });
        response.end(request.method === "HEAD" ? undefined : content);
    }

    private async serveLocal(
        request: IncomingMessage,
        response: ServerResponse,
        url: URL,
        routePath: string,
    ): Promise<void> {
        const raw = routePath.slice("local/".length),
            slash = raw.indexOf("/");
        if (slash < 1 || slash === raw.length - 1) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const capabilityToken = raw.slice(0, slash),
            relativePath = raw.slice(slash + 1);
        let configuredCapability: string | undefined = undefined,
            configuredUrl: string | undefined = undefined;
        for (const profile of this.config.profiles) {
            for (const placement of profile.overlays) {
                if (!placement.url) {
                    continue;
                }
                const capability = localCapability(placement.url);
                if (capability?.startsWith(`${capabilityToken}/`)) {
                    if (configuredCapability !== undefined && capability !== configuredCapability) {
                        sendText(response, 404, "Not Found\n");
                        return;
                    }
                    configuredCapability = capability;
                    configuredUrl = placement.url;
                }
            }
        }
        if (
            configuredUrl === undefined ||
            configuredCapability === undefined ||
            !this.options.registry.authorizeUrl(configuredUrl)
        ) {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const root = configuredCapability.slice(configuredCapability.indexOf("/") + 1),
            localUrl = `vosulay-overlay://${capabilityToken}/${root}/${relativePath}${url.search}`;
        let path: string;
        try {
            path = await this.options.registry.resolveAuthorizedFile(localUrl);
        } catch {
            sendText(response, 404, "Not Found\n");
            return;
        }
        const info = await stat(path),
            contentType =
                mime.contentType(mime.lookup(path) || "application/octet-stream") ||
                "application/octet-stream";
        response.writeHead(200, {
            "access-control-allow-origin": "null",
            "cache-control": "no-store",
            "content-length": String(info.size),
            "content-type": contentType,
            "cross-origin-resource-policy": "cross-origin",
            "referrer-policy": "no-referrer",
            "x-content-type-options": "nosniff",
        });
        if (request.method === "HEAD") {
            response.end();
            return;
        }
        await pipeline(createReadStream(path), response);
    }
}
