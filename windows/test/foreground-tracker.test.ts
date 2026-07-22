import assert from "node:assert/strict";
import test from "node:test";

import {
    type ActiveWindowApi,
    type ActiveWindowInfo,
    WindowsForegroundTracker,
} from "../src/main/foreground-tracker";

test("foreground tracker seeds after subscribing and polls through missed hooks", () => {
    let activePid = 11;
    let subscription: ((windowInfo: ActiveWindowInfo | null) => void) | null = null;
    let poll: (() => void) | null = null;
    let initialized = false;
    let unsubscribed: number | null = null;
    let readbackFails = false;
    let readbackCount = 0;
    let pollCancelled = false;
    const activeWindow: ActiveWindowApi = {
        initialize: () => {
            initialized = true;
        },
        subscribe: (callback) => {
            subscription = callback;
            // Simulate a focus switch in the native hook startup window without an event.
            activePid = 22;
            return 7;
        },
        getActiveWindow: () => {
            readbackCount += 1;
            if (readbackFails) {
                throw new Error("no active window");
            }
            return { pid: activePid };
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
                poll = null;
            };
        },
    });
    const notifications: Array<number | null> = [];

    assert.equal(tracker.start((pid) => notifications.push(pid)), 22);
    assert.equal(initialized, true);
    assert.equal(readbackCount, 1);
    assert.ok(subscription);
    assert.ok(poll);

    subscription({ pid: 33 });
    subscription({ pid: 33 });
    assert.deepEqual(notifications, [33]);

    activePid = 44;
    poll();
    poll();
    assert.deepEqual(notifications, [33, 44]);

    readbackFails = true;
    poll();
    poll();
    assert.deepEqual(notifications, [33, 44, null]);

    tracker.stop();
    assert.equal(unsubscribed, 7);
    assert.equal(pollCancelled, true);
});
