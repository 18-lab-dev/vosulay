export type EditShortcutCommand = "toggle-interaction" | "refresh-selected-overlay";

export interface EditShortcutInput {
    type: string;
    key: string;
    isAutoRepeat: boolean;
    control: boolean;
    alt: boolean;
    meta: boolean;
    shift: boolean;
}

export function editShortcutCommand(input: EditShortcutInput): EditShortcutCommand | undefined {
    if (
        input.type !== "keyDown" ||
        input.isAutoRepeat ||
        !input.control ||
        input.alt ||
        input.meta ||
        input.shift
    ) {
        return undefined;
    }

    switch (input.key.toLowerCase()) {
        case "i": {
            return "toggle-interaction";
        }
        case "r": {
            return "refresh-selected-overlay";
        }
        default: {
            return undefined;
        }
    }
}

export interface NativeEditShortcutInput {
    kind: string;
    key?: {
        code: number;
    };
    state?: string;
}

const VK_ALT = 0x12;
const VK_APPS = 0x5d;
const VK_CONTROL = 0x11;
const VK_I = 0x49;
const VK_LEFT_ALT = 0xa4;
const VK_LEFT_CONTROL = 0xa2;
const VK_LEFT_META = 0x5b;
const VK_LEFT_SHIFT = 0xa0;
const VK_R = 0x52;
const VK_RIGHT_ALT = 0xa5;
const VK_RIGHT_CONTROL = 0xa3;
const VK_RIGHT_META = 0x5c;
const VK_RIGHT_SHIFT = 0xa1;
const VK_SHIFT = 0x10;

export class NativeEditShortcutState {
    private readonly pressedKeys = new Set<number>();

    handle(input: NativeEditShortcutInput): EditShortcutCommand | undefined {
        if (input.kind !== "Key" || input.key === undefined) {
            return undefined;
        }

        const { code } = input.key;
        if (input.state === "Released") {
            this.pressedKeys.delete(code);
            return undefined;
        }
        if (input.state !== "Pressed") {
            return undefined;
        }

        const repeated = this.pressedKeys.has(code);
        this.pressedKeys.add(code);
        if (
            repeated ||
            !this.hasAny(VK_CONTROL, VK_LEFT_CONTROL, VK_RIGHT_CONTROL) ||
            this.hasAny(VK_SHIFT, VK_LEFT_SHIFT, VK_RIGHT_SHIFT) ||
            this.hasAny(VK_ALT, VK_LEFT_ALT, VK_RIGHT_ALT) ||
            this.hasAny(VK_LEFT_META, VK_RIGHT_META, VK_APPS)
        ) {
            return undefined;
        }

        if (code === VK_I) {
            return "toggle-interaction";
        }
        if (code === VK_R) {
            return "refresh-selected-overlay";
        }
        return undefined;
    }

    clear(): void {
        this.pressedKeys.clear();
    }

    private hasAny(first: number, second: number, third?: number): boolean {
        return (
            this.pressedKeys.has(first) ||
            this.pressedKeys.has(second) ||
            (third !== undefined && this.pressedKeys.has(third))
        );
    }
}
