export type FatalExit = (message: string) => never;

export function fatalProcessExit(message: string): never {
    console.error(message);
    process.exit(1);
}

export async function withFatalDeadline(
    operation: () => PromiseLike<void> | void,
    timeoutMs: number,
    message: string,
    fatalExit: FatalExit = fatalProcessExit
): Promise<void> {
    let timer: NodeJS.Timeout | null = null;
    const timeout = new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => {
            try {
                fatalExit(message);
            } catch (error) {
                reject(error);
            }
        }, timeoutMs);
    });

    try {
        await Promise.race([
            timeout,
            Promise.resolve().then(operation)
        ]);
    } finally {
        if (timer !== null) {
            clearTimeout(timer);
        }
    }
}
