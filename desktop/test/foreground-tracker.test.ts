import assert from "node:assert/strict";
import test from "node:test";

import {
    type ActiveWindowApi,
    type ActiveWindowInfo,
    WindowsForegroundTracker,
} from "../src/main/foreground-tracker";

test("foreground tracker seeds after subscribing and polls through missed hooks", () => {
    let activePid = 11,
        initialized = false,
        poll,
        pollCancelled = false,
        readbackCount = 0,
        readbackFails = false,
        subscription,
        unsubscribed;
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
        },
        tracker = new WindowsForegroundTracker({
            loadActiveWindow: () => activeWindow,
            scheduleReadback: (callback) => {
                poll = callback;
                return () => {
                    pollCancelled = true;
                    poll = undefined;
                };
            },
        }),
        notifications: (number | undefined)[] = [];

    assert.equal(
        tracker.start((pid) => notifications.push(pid)),
        22,
    );
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
    assert.deepEqual(notifications, [33, 44, undefined]);

    tracker.stop();
    assert.equal(unsubscribed, 7);
    assert.equal(pollCancelled, true);
});
