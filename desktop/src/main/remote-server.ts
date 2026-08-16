import { type AddressInfo, isIP } from "node:net";
import { type AppConfig, type Placement, type Profile } from "./types";
import { type IncomingMessage, type Server, type ServerResponse, createServer } from "node:http";
import { type LocalOverlayRegistry } from "./local-overlays";
import { type RuntimeOptions } from "./options";
import { counterUrl } from "./urls";
import { createReadStream } from "node:fs";
import mime from "mime-types";
import { once } from "node:events";
import { pipeline } from "node:stream/promises";
import { stat } from "node:fs/promises";
import { timingSafeEqual } from "node:crypto";
const MAX_ACTIVE_REQUESTS = 32;
const BYTES_PER_KIB = 1024;
const MAX_REQUEST_HEADER_KIB = 16;
const MAX_REQUEST_HEADER_BYTES = MAX_REQUEST_HEADER_KIB * BYTES_PER_KIB;
const REQUEST_TIMEOUT_MS = 3000;
const OVERLAY_CSP =
    "default-src 'none'; frame-src 'self' http: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors *";
const OVERLAY_PERMISSIONS_POLICY = "webgpu=*";
const HTTP_STATUS_OK = 200;
const HTTP_STATUS_NOT_FOUND = 404;
const HTTP_STATUS_FORBIDDEN = 403;
const HTTP_STATUS_BAD_REQUEST = 400;
const HTTP_STATUS_METHOD_NOT_ALLOWED = 405;
const HTTP_STATUS_REQUEST_TIMEOUT = 408;
const HTTP_STATUS_INTERNAL_SERVER_ERROR = 500;
const HTTP_STATUS_SERVICE_UNAVAILABLE = 503;
const HTTP_STATUS_MAX_REQUESTS_PER_SOCKET = 100;
const IPV4_VERSION = 4;
const IPV6_VERSION = 6;
const IPV4_PART_COUNT = 4;
const IPV6_PART_COUNT = 8;
const IPV6_DOUBLE_COLON_PARTS = 2;
const IPV6_MAPPED_PREFIX_PART_COUNT = 5;
const IPV6_SHIFT_BITS = 8;
const DECIMAL_RADIX = 10;
const HEXADECIMAL_RADIX = 16;
const IPV6_SEGMENT_MASK = 0xff_ff;
const IPV6_BYTE_MASK = 0xff;
const IPV4_LOOPBACK_SEGMENT = 127;
const MAX_IPV4_SEGMENT = 0xff;
const IPV6_MAPPED_PREFIX_INDEX = 5;
const IPV4_THIRD_OCTET_INDEX = 2;
const IPV4_FOURTH_OCTET_INDEX = 3;
const IPV6_EMBEDDED_IPV4_HIGH_PART_INDEX = 6;
const IPV6_EMBEDDED_IPV4_LOW_PART_INDEX = 7;
const IPV6_LOOPBACK_REQUIRED_SEGMENTS = 7;
const MAX_SOCKETS_PER_REQUEST = 2;
const CAPABILITY_PATH_SEGMENT_COUNT = 3;
const PROFILE_QUERY_NAME = "profile";
const PROFILE_QUERY_SEPARATOR = "=";
const LOCAL_ACCESS_PREFIX = "/access/";
const LOCAL_ACCESS_PREFIX_LENGTH = LOCAL_ACCESS_PREFIX.length;
const LOCAL_PATH_PREFIX = "local/";
const LOCAL_PATH_PREFIX_LENGTH = LOCAL_PATH_PREFIX.length;
const LOCAL_PATH_SEGMENT_INDEX = 2;
const LOCAL_PATH_SEPARATOR = "/";
const ROOT_PATTERN = /^[A-Za-z0-9_-]+$/u;
const TOKEN_PATTERN = /^[0-9a-f]{32}$/u;
const DEFAULT_HOST = "0.0.0.0";
const DEFAULT_PORT = 5050;
const ACCESS_TOKEN_PATTERN = /^[0-9a-f]{64}$/u;
const RAW_HEADER_FIELD_PAIR_COUNT = 2;
const UTF8_ENCODING = "utf8";
type ProfileSelectionStatus = typeof HTTP_STATUS_BAD_REQUEST | typeof HTTP_STATUS_NOT_FOUND;

export interface RemoteOverlayServerOptions {
    runtime: RuntimeOptions;
    registry: LocalOverlayRegistry;
    initialConfig: AppConfig;
    host?: string;
    port?: number;
}

function isAddressInfo(address: AddressInfo | string | undefined): address is AddressInfo {
    return (
        typeof address === "object" && address !== null && "address" in address && "port" in address
    );
}

function ipv4Parts(value: string): number[] | undefined {
    if (isIP(value) !== IPV4_VERSION) {
        return undefined;
    }
    return value.split(".").map(Number);
}

function ipv6Parts(value: string): number[] | undefined {
    if (isIP(value) !== IPV6_VERSION || value.includes("%")) {
        return undefined;
    }

    let source = value.toLowerCase();
    const dotted = source.lastIndexOf(":");
    if (source.includes(".") && dotted !== -1) {
        const ipv4 = ipv4Parts(source.slice(dotted + 1));
        if (ipv4 === undefined) {
            return undefined;
        }

        const ipv4HighWord = ((ipv4[0] ?? 0) << IPV6_SHIFT_BITS) | (ipv4[1] ?? 0);
        const ipv4LowWord =
            ((ipv4[IPV4_THIRD_OCTET_INDEX] ?? 0) << IPV6_SHIFT_BITS) |
            (ipv4[IPV4_FOURTH_OCTET_INDEX] ?? 0);
        source = `${source.slice(0, dotted)}:${ipv4HighWord.toString(HEXADECIMAL_RADIX)}:${ipv4LowWord.toString(HEXADECIMAL_RADIX)}`;
    }

    const halves = source.split("::");
    if (halves.length > IPV6_DOUBLE_COLON_PARTS) {
        return undefined;
    }

    const left = halves[0] ? halves[0].split(":") : [];
    const right = halves[1] ? halves[1].split(":") : [];
    const missing = IPV6_PART_COUNT - left.length - right.length;

    if (
        (halves.length === 1 && missing !== 0) ||
        (halves.length === IPV6_DOUBLE_COLON_PARTS && missing < 1)
    ) {
        return undefined;
    }

    const words = [...left, ...Array.from({ length: missing }, () => "0"), ...right].map((part) =>
        Number.parseInt(part, HEXADECIMAL_RADIX),
    );
    return words.length === IPV6_PART_COUNT &&
        words.every((part) => Number.isInteger(part) && part >= 0 && part <= IPV6_SEGMENT_MASK)
        ? words
        : undefined;
}

function normalizeIp(value: string): string | undefined {
    const version = isIP(value);
    if (version === IPV4_VERSION) {
        return value;
    }
    if (version !== IPV6_VERSION) {
        return undefined;
    }

    const parts = ipv6Parts(value);
    if (parts === undefined) {
        return undefined;
    }

    if (
        parts.slice(0, IPV6_MAPPED_PREFIX_PART_COUNT).every((part) => part === 0) &&
        parts[IPV6_MAPPED_PREFIX_INDEX] === IPV6_SEGMENT_MASK
    ) {
        return `${(parts[IPV6_EMBEDDED_IPV4_HIGH_PART_INDEX] ?? 0) >> IPV6_SHIFT_BITS}.${(parts[IPV6_EMBEDDED_IPV4_HIGH_PART_INDEX] ?? 0) & IPV6_BYTE_MASK}.${(parts[IPV6_EMBEDDED_IPV4_LOW_PART_INDEX] ?? 0) >> IPV6_SHIFT_BITS}.${(parts[IPV6_EMBEDDED_IPV4_LOW_PART_INDEX] ?? 0) & IPV6_BYTE_MASK}`;
    }

    return value;
}

function sameIp(left: string, right: string): boolean {
    const normalizedLeft = normalizeIp(left);
    const normalizedRight = normalizeIp(right);

    if (normalizedLeft === undefined || normalizedRight === undefined) {
        return false;
    }

    const version = isIP(normalizedLeft);
    if (version !== isIP(normalizedRight)) {
        return false;
    }

    const leftParts =
        version === IPV4_VERSION ? ipv4Parts(normalizedLeft) : ipv6Parts(normalizedLeft);
    const rightParts =
        version === IPV4_VERSION ? ipv4Parts(normalizedRight) : ipv6Parts(normalizedRight);

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
        return ipv4[0] === IPV4_LOOPBACK_SEGMENT;
    }

    const ipv6 = ipv6Parts(normalized);
    return (
        ipv6 !== undefined &&
        ipv6.slice(0, IPV6_LOOPBACK_REQUIRED_SEGMENTS).every((part) => part === 0) &&
        ipv6[IPV6_LOOPBACK_REQUIRED_SEGMENTS] === 1
    );
}

function matchesIpv4Pattern(address: string, pattern: string): boolean {
    const normalized = normalizeIp(address);
    const addressParts = normalized === undefined ? undefined : ipv4Parts(normalized);
    const patternParts = pattern.split(".");

    if (addressParts === undefined || patternParts.length !== IPV4_PART_COUNT) {
        return false;
    }

    return patternParts.every((part, index) => {
        if (part === "*") {
            return true;
        }

        if (!/^\d+$/u.test(part)) {
            return false;
        }

        const octet = Number.parseInt(part, DECIMAL_RADIX);
        return (
            octet >= 0 &&
            octet <= MAX_IPV4_SEGMENT &&
            String(octet) === part &&
            octet === addressParts[index]
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
        /[&<>"']/gu,
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
        const url = new URL(value);
        const parts = url.pathname.split("/");

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
            parts.length < CAPABILITY_PATH_SEGMENT_COUNT ||
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

    const url = new URL(value);
    const pathSegments = url.pathname.split("/");
    const relativeSegments = pathSegments.slice(LOCAL_PATH_SEGMENT_INDEX);

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

function renderPlacement(
    placement: Placement,
    vosuUrl: URL,
    credential: string,
): string | undefined {
    const resolved = placementSource(placement, vosuUrl, credential);
    if (resolved === undefined) {
        return undefined;
    }

    const title = placement.name?.trim() || placement.counter_id || "Overlay";
    const style = `position:absolute;left:${placement.left}px;top:${placement.top}px;width:${placement.width}px;height:${placement.height}px;transform-origin:0 0;transform:scale(${placement.scale}) rotate(${placement.rotation}deg);opacity:${placement.opacity};z-index:${placement.z_index};border:0;background:transparent`;

    let sandbox = "";
    if (resolved.kind === "local") {
        sandbox = ' sandbox="allow-scripts"';
    } else if (resolved.kind === "remote") {
        sandbox = ' sandbox="allow-scripts allow-same-origin"';
    }

    /*
     * Arbitrary remote URLs are untrusted content. They retain ordinary media
     * access but do not receive GPU delegation by default. Installed counters
     * and HMAC-authenticated local overlays are explicitly trusted sources.
     */
    const allow = resolved.kind === "remote" ? "autoplay" : "autoplay; webgpu";
    return `<iframe src="${htmlEscape(resolved.source)}" title="${htmlEscape(title)}" allow="${allow}" loading="eager"${sandbox} referrerpolicy="no-referrer" style="${style}"></iframe>`;
}

export function renderProfileDocument(
    profile: Profile,
    runtime: RuntimeOptions,
    credential: string,
    requestHost?: string,
): string {
    const vosuUrl = vosuUrlForRequest(runtime.vosuUrl, requestHost);
    const frames = profile.overlays
        .map((placement) => renderPlacement(placement, vosuUrl, credential))
        .filter((frame): frame is string => frame !== undefined)
        .join("\n");

    return `<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${htmlEscape(profile.name)}</title><style>*{box-sizing:border-box}html,body,#stage{width:100%;height:100%;margin:0;overflow:hidden;background:transparent}#stage{position:fixed;inset:0}</style></head><body><main id="stage">${frames}</main></body></html>`;
}

class ProfileSelectionError extends Error {
    readonly status: ProfileSelectionStatus;

    constructor(status: ProfileSelectionStatus, message: string) {
        super(message);
        this.name = "ProfileSelectionError";
        this.status = status;
        Object.setPrototypeOf(this, ProfileSelectionError.prototype);
    }
}

function decodeProfileQueryPart(part: string): string {
    return decodeURIComponent(part.replaceAll("+", " "));
}

function selectProfile(url: URL, config: AppConfig): Profile | undefined | ProfileSelectionError {
    const rawQuery = url.search.startsWith("?") ? url.search.slice(1) : "";
    if (rawQuery === "") {
        return config.profiles.find((profile) => profile.id === config.active_profile) ?? undefined;
    }

    const parameters = rawQuery.split("&");
    if (parameters.length !== 1) {
        return new ProfileSelectionError(
            HTTP_STATUS_BAD_REQUEST,
            "profile must be specified exactly once",
        );
    }

    const separator = parameters[0]?.indexOf(PROFILE_QUERY_SEPARATOR) ?? -1;
    if (separator < 0) {
        return new ProfileSelectionError(HTTP_STATUS_BAD_REQUEST, "malformed profile query");
    }

    let name = "";
    let value = "";

    try {
        name = decodeProfileQueryPart(parameters[0]?.slice(0, separator) ?? "");
        value = decodeProfileQueryPart(parameters[0]?.slice(separator + 1) ?? "");
    } catch {
        return new ProfileSelectionError(HTTP_STATUS_BAD_REQUEST, "malformed profile query");
    }

    if (name !== PROFILE_QUERY_NAME) {
        return new ProfileSelectionError(
            HTTP_STATUS_BAD_REQUEST,
            "profile must be the only query parameter",
        );
    }

    if (!/^[1-9]\d*$/u.test(value)) {
        return new ProfileSelectionError(
            HTTP_STATUS_BAD_REQUEST,
            "profile must be a positive 1-based integer",
        );
    }

    const index = Number(value);
    if (!Number.isSafeInteger(index)) {
        return new ProfileSelectionError(HTTP_STATUS_BAD_REQUEST, "profile index is too large");
    }

    if (index > config.profiles.length) {
        return new ProfileSelectionError(
            HTTP_STATUS_NOT_FOUND,
            `profile index ${value} is out of range`,
        );
    }

    return config.profiles[index - 1] ?? undefined;
}

function sendText(
    response: ServerResponse,
    status: number,
    body: string,
    headers: Record<string, string> = {},
): void {
    const content = Buffer.from(body, UTF8_ENCODING);
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
    const protectedPath = pathname.startsWith(LOCAL_ACCESS_PREFIX)
        ? pathname.slice(LOCAL_ACCESS_PREFIX_LENGTH)
        : "";
    const separator = protectedPath.indexOf(LOCAL_PATH_SEPARATOR);

    if (separator < 1) {
        return undefined;
    }

    const credential = protectedPath.slice(0, separator);
    const path = protectedPath.slice(separator + 1);
    return path === "overlay" || path.startsWith(LOCAL_PATH_PREFIX)
        ? { credential, path }
        : undefined;
}

function credentialMatches(provided: string, expected: string): boolean {
    if (!ACCESS_TOKEN_PATTERN.test(provided) || !ACCESS_TOKEN_PATTERN.test(expected)) {
        return false;
    }
    return timingSafeEqual(Buffer.from(provided, "ascii"), Buffer.from(expected, "ascii"));
}

/*
 * A header present more than once is malformed and must be rejected. This
 * sentinel distinguishes that from an absent header, which returns undefined.
 */
const MALFORMED_HEADER = Symbol("malformed-header");

function singleHeader(
    request: IncomingMessage,
    name: string,
): string | undefined | typeof MALFORMED_HEADER {
    const values: string[] = [];
    for (let index = 0; index < request.rawHeaders.length; index += RAW_HEADER_FIELD_PAIR_COUNT) {
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
    const { localAddress, localPort } = request.socket;

    if (localAddress === undefined || localPort === undefined) {
        return undefined;
    }

    try {
        const url = new URL(`http://${host}/`);
        const hostname =
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
    if (host === MALFORMED_HEADER || host === undefined || !host) {
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
                    sendText(response, HTTP_STATUS_SERVICE_UNAVAILABLE, "Server is busy\n");
                    return;
                }

                this.activeRequests += 1;
                request.setTimeout(REQUEST_TIMEOUT_MS, () => {
                    if (response.headersSent) {
                        response.destroy();
                    } else {
                        sendText(response, HTTP_STATUS_REQUEST_TIMEOUT, "Request Timeout\n");
                    }
                });

                response.setTimeout(REQUEST_TIMEOUT_MS, () => response.destroy());
                void this.handle(request, response)
                    .catch(() => {
                        if (response.headersSent) {
                            response.destroy();
                        } else {
                            sendText(
                                response,
                                HTTP_STATUS_INTERNAL_SERVER_ERROR,
                                "Internal Server Error\n",
                            );
                        }
                    })
                    .finally(() => {
                        this.activeRequests -= 1;
                    });
            },
        );

        this.server.maxConnections = MAX_ACTIVE_REQUESTS * MAX_SOCKETS_PER_REQUEST;
        this.server.maxRequestsPerSocket = HTTP_STATUS_MAX_REQUESTS_PER_SOCKET;
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
        const address = this.server.address();
        return address !== null && isAddressInfo(address) ? address : undefined;
    }

    private async handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
        if (request.method !== "GET" && request.method !== "HEAD") {
            sendText(response, HTTP_STATUS_METHOD_NOT_ALLOWED, "Method Not Allowed\n", {
                allow: "GET, HEAD",
            });
            return;
        }

        const url = new URL(request.url ?? "/", "http://localhost");
        const route = protectedRoute(url.pathname);

        if (route === undefined) {
            sendText(response, HTTP_STATUS_NOT_FOUND, "Not Found\n");
            return;
        }

        if (
            !requestMetadataIsAllowed(
                request,
                this.config.settings.remote_access_host,
                route.path.startsWith(LOCAL_PATH_PREFIX),
            ) ||
            !isPeerAllowed(request.socket.remoteAddress, this.config.settings.allowed_ips) ||
            !credentialMatches(route.credential, this.config.settings.remote_access_token)
        ) {
            sendText(response, HTTP_STATUS_FORBIDDEN, "Forbidden\n");
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
            sendText(response, HTTP_STATUS_NOT_FOUND, "Profile not found\n");
            return;
        }

        const host = singleHeader(request, "host");
        const requestHost = host === MALFORMED_HEADER ? undefined : host;
        const content = Buffer.from(
            renderProfileDocument(selected, this.options.runtime, credential, requestHost),
            UTF8_ENCODING,
        );

        response.writeHead(HTTP_STATUS_OK, {
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
        const raw = routePath.slice(LOCAL_PATH_PREFIX_LENGTH);
        const slash = raw.indexOf(LOCAL_PATH_SEPARATOR);

        if (slash < 1 || slash === raw.length - 1) {
            sendText(response, HTTP_STATUS_NOT_FOUND, "Not Found\n");
            return;
        }

        const capabilityToken = raw.slice(0, slash);
        const relativePath = raw.slice(slash + 1);
        let configuredCapability: string | undefined = undefined;
        let configuredUrl: string | undefined = undefined;

        for (const profile of this.config.profiles) {
            for (const placement of profile.overlays) {
                if (!placement.url) {
                    continue;
                }

                const capability = localCapability(placement.url);
                if (capability?.startsWith(`${capabilityToken}/`)) {
                    if (configuredCapability !== undefined && capability !== configuredCapability) {
                        sendText(response, HTTP_STATUS_NOT_FOUND, "Not Found\n");
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
            sendText(response, HTTP_STATUS_NOT_FOUND, "Not Found\n");
            return;
        }
        const root = configuredCapability.slice(
            configuredCapability.indexOf(LOCAL_PATH_SEPARATOR) + 1,
        );
        const localUrl = `vosulay-overlay://${capabilityToken}/${root}/${relativePath}${url.search}`;
        let path = "";

        try {
            path = await this.options.registry.resolveAuthorizedFile(localUrl);
        } catch {
            sendText(response, HTTP_STATUS_NOT_FOUND, "Not Found\n");
            return;
        }

        const info = await stat(path);
        const contentType =
            mime.contentType(mime.lookup(path) || "application/octet-stream") ||
            "application/octet-stream";

        response.writeHead(HTTP_STATUS_OK, {
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
