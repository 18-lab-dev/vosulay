import { type ActiveWindowApi, WindowsForegroundTracker } from "../src/main/foreground-tracker";
import assert from "node:assert/strict";
import test from "node:test";

const noop = (): void => {};
const noopWindowSubscription = (_windowInfo: { pid: number }): void => {};

test("foreground tracker seeds after subscribing and polls through missed hooks", () => {
    let activePid = 11;
    let initialized = false;
    let poll: () => void = noop;
    let pollCancelled = false;
    let readbackCount = 0;
    let readbackFails = false;
    let subscription: (windowInfo: { pid: number }) => void = noopWindowSubscription;
    let unsubscribed = 0;
    const activeWindow: ActiveWindowApi = {
        getActiveWindow: () => {
            readbackCount += 1;
            if (readbackFails) {
                throw new Error("no active window");
            }
            return { pid: activePid };
        },
        initialize: () => {
            initialized = true;
        },
        subscribe: (callback) => {
            subscription = callback;
            // Simulate a focus switch in the native hook startup window without an event.
            activePid = 22;
            return 7;
        },
        unsubscribe: (watchId) => {
            unsubscribed = watchId;
        },
    };
    const tracker = new WindowsForegroundTracker({
        loadActiveWindow: () => activeWindow,
        scheduleReadback: (callback) => {
            poll = callback;
            return () => {
                pollCancelled = true;
                poll = noop;
            };
        },
    });
    const notifications: (number | undefined)[] = [];

    assert.equal(
        tracker.start((pid) => notifications.push(pid)),
        22,
    );
    assert.equal(initialized, true);
    assert.equal(readbackCount, 1);
    assert.ok(subscription);
    assert.ok(poll);

    subscription?.({ pid: 33 });
    subscription?.({ pid: 33 });
    assert.deepEqual(notifications, [33]);

    activePid = 44;
    poll?.();
    poll?.();
    assert.deepEqual(notifications, [33, 44]);

    readbackFails = true;
    poll?.();
    poll?.();
    assert.deepEqual(notifications, [33, 44, undefined]);

    tracker.stop();
    assert.equal(unsubscribed, 7);
    assert.equal(pollCancelled, true);
});
