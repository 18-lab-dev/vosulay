import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const BOOTSTRAP_MARKER = "__VOSULAY_BOOTSTRAP__";

export function encodeJavaScriptString(source: string): string {
    return JSON.stringify(source)
        .replaceAll("<", "\\u003c")
        .replaceAll(">", "\\u003e")
        .replaceAll("&", "\\u0026")
        .replaceAll("\u2028", "\\u2028")
        .replaceAll("\u2029", "\\u2029");
}

function webGpuDelegationGuard(trustedCounterOrigin: string): string {
    const origin = encodeJavaScriptString(trustedCounterOrigin);
    return `(()=>{const trusted=${origin};const original=HTMLIFrameElement.prototype.setAttribute;HTMLIFrameElement.prototype.setAttribute=function(name,value){if(String(name).toLowerCase()==="allow"&&typeof value==="string"&&/(?:^|;)\\s*webgpu\\s*(?:;|$)/i.test(value)){let permitted=false;try{const source=new URL(this.src||this.getAttribute("src")||"",document.baseURI);permitted=source.protocol==="vosulay-overlay:"||Boolean(trusted)&&source.origin===trusted;}catch{}if(!permitted){value=value.split(";").map(part=>part.trim()).filter(part=>part&&part.toLowerCase()!=="webgpu").join("; ");}}return original.call(this,name,value);};})();`;
}

export function buildManagerDocumentFromSources(
    template: string,
    css: string,
    javascript: string,
    fpsLimit: number,
    trustedCounterOrigin = "",
): string {
    if (!Number.isInteger(fpsLimit) || fpsLimit <= 0 || fpsLimit > 240) throw new Error("manager FPS limit must be between 1 and 240");
    const occurrences = template.split(BOOTSTRAP_MARKER).length - 1;
    if (occurrences !== 1) throw new Error(`manager template must contain exactly one ${BOOTSTRAP_MARKER} marker`);
    const bootstrap = `window.__VOSULAY_FPS_LIMIT__=${fpsLimit};(()=>{const style=document.createElement("style");style.textContent=${encodeJavaScriptString(css)};document.head.appendChild(style);${webGpuDelegationGuard(trustedCounterOrigin)}(0,eval)(${encodeJavaScriptString(javascript)});})();`;
    return template.replace(BOOTSTRAP_MARKER, bootstrap);
}

function loadManagerAsset(filename: string): string {
    const moduleAsset = fileURLToPath(new URL(`../../../assets/${filename}`, import.meta.url));
    try {
        return readFileSync(moduleAsset, "utf8");
    } catch (moduleError) {
        const resourcesPath = (process as NodeJS.Process & { resourcesPath?: string }).resourcesPath;
        if (resourcesPath) {
            try {
                return readFileSync(join(resourcesPath, "assets", filename), "utf8");
            } catch {
                // Report the original source-tree path, which is actionable in development.
            }
        }
        throw moduleError;
    }
}

export function buildManagerDocument(fpsLimit: number, trustedCounterOrigin = ""): string {
    return buildManagerDocumentFromSources(
        loadManagerAsset("manager.html"),
        loadManagerAsset("manager.css"),
        loadManagerAsset("manager.js"),
        fpsLimit,
        trustedCounterOrigin,
    );
}
