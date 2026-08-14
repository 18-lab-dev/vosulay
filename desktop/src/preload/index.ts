import { contextBridge, ipcRenderer, webFrame } from 'electron';

const MANAGER_URL = 'vosulay://manager/index.html';
const BRIDGE_CHANNEL = 'vosulay:message';

declare global {
    interface Window {
        __VOSULAY_POST_MESSAGE__?: (payload: string) => void;
        __VOSULAY_RECEIVE__?: (envelope: unknown) => void;
    }
}

if (process.isMainFrame && window.location.href === MANAGER_URL) {
    const deliver = (envelope: unknown): void => {
        const serialized = JSON.stringify(envelope)
            .replaceAll('<', '\\u003c')
            .replaceAll('\u2028', '\\u2028')
            .replaceAll('\u2029', '\\u2029');
        void webFrame.executeJavaScript(
            `window.__VOSULAY_RECEIVE__?.(${serialized});`
        );
    };

    contextBridge.exposeInMainWorld(
        '__VOSULAY_POST_MESSAGE__',
        (payload: string): void => {
            if (typeof payload !== 'string') {
                throw new TypeError('vosulay bridge payload must be a string');
            }
            ipcRenderer.send(BRIDGE_CHANNEL, payload);
        }
    );

    ipcRenderer.on(BRIDGE_CHANNEL, (_event, envelope: unknown) => {
        deliver(envelope);
    });
    window.dispatchEvent(new Event('vosulay-bridge-ready'));
}
