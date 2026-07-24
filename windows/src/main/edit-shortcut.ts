export type EditShortcutCommand =
    | "toggle-interaction"
    | "refresh-selected-overlay";

export interface EditShortcutInput {
    type: string;
    key: string;
    isAutoRepeat: boolean;
    control: boolean;
    alt: boolean;
    meta: boolean;
    shift: boolean;
}

export function editShortcutCommand(
    input: EditShortcutInput,
): EditShortcutCommand | null {
    if (
        input.type !== "keyDown"
        || input.isAutoRepeat
        || !input.control
        || input.alt
        || input.meta
        || input.shift
    ) {
        return null;
    }

    switch (input.key.toLowerCase()) {
        case "i":
            return "toggle-interaction";
        case "r":
            return "refresh-selected-overlay";
        default:
            return null;
    }
}
