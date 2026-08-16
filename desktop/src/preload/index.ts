import { type JsonValue, isJsonString } from "../main/json";
import { contextBridge, ipcRenderer, webFrame } from "electron";
const BRIDGE_CHANNEL = "vosulay:message";
const MANAGER_URL = "vosulay://manager/index.html";

declare global {
    interface Window {
        __VOSULAY_POST_MESSAGE__?: (payload: string) => void;
        __VOSULAY_RECEIVE__?: (envelope: JsonValue) => void;
    }
}

function deliver(envelope: JsonValue): void {
    const serialized = JSON.stringify(envelope)
        .replaceAll("<", String.raw`\u003c`)
        .replaceAll("\u2028", String.raw`\u2028`)
        .replaceAll("\u2029", String.raw`\u2029`);
    void webFrame.executeJavaScript(`window.__VOSULAY_RECEIVE__?.(${serialized});`);
}

if (process.isMainFrame && globalThis.location.href === MANAGER_URL) {
    contextBridge.exposeInMainWorld("__VOSULAY_POST_MESSAGE__", (payload: JsonValue): void => {
        if (!isJsonString(payload)) {
            throw new TypeError("vosulay bridge payload must be a string");
        }
        ipcRenderer.send(BRIDGE_CHANNEL, payload);
    });

    ipcRenderer.on(BRIDGE_CHANNEL, (_event, envelope: JsonValue): void => {
        deliver(envelope);
    });
    globalThis.dispatchEvent(new Event("vosulay-bridge-ready"));
}
