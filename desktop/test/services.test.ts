import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { mkdir, mkdtemp, readFile, realpath, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { ConfigStore, normalizeConfig } from "../src/main/config";
import {
    MAX_COUNTER_PAGE_BYTES,
    fetchInstalledCounters,
    parseInstalledCounters,
} from "../src/main/counters";
import { HostBridge, localOverlayUrls } from "../src/main/host-bridge";
import { withFatalDeadline } from "../src/main/fatal-deadline";
import { NativeEditShortcutState, editShortcutCommand } from "../src/main/edit-shortcut";
import { LocalOverlayRegistry } from "../src/main/local-overlays";
import { chooseOverlayTarget } from "../src/main/overlay-target";
import { buildManagerDocumentFromSources } from "../src/main/manager-document";
import { parseInvocation, parseOptions } from "../src/main/options";
import { resolveStoragePaths } from "../src/main/storage-path";
import { availableUrl, counterUrl, settingsUrl } from "../src/main/urls";
import type { AppSettings, MessageEnvelope } from "../src/main/types";

const TEST_REMOTE_ACCESS_TOKEN = "0123456789abcdef".repeat(4);

function redactedRemoteAccessToken(settings: AppSettings): AppSettings {
    return { ...settings, remote_access_token: "[credential]" };
}

test("config path resolver selects installed and portable storage", () => {
    const appDataDirectory = resolve("appdata"),
        portableExecutableDirectory = resolve("portable");
    assert.deepEqual(resolveStoragePaths(appDataDirectory), {
        configPath: join(appDataDirectory, "vosulay", "config.json"),
        dataDirectory: join(appDataDirectory, "vosulay"),
    });
    assert.deepEqual(resolveStoragePaths(appDataDirectory, portableExecutableDirectory), {
        configPath: join(portableExecutableDirectory, "data", "config.json"),
        dataDirectory: join(portableExecutableDirectory, "data"),
    });
    assert.throws(
        () => resolveStoragePaths("/appdata", ""),
        /portable executable directory must not be empty/,
    );
    assert.throws(
        () => resolveStoragePaths("/appdata", "portable"),
        /portable executable directory must be absolute/,
    );
});

test("options parse aliases and reject unsafe values without Electron", () => {
    const options = parseOptions([
        "-t",
        "https://example.test/vosu",
        "-f144",
        "--pid=42",
        "--pid",
        "99",
        "--process-name",
        "custom.exe",
    ]);
    assert.equal(options.vosuUrl.href, "https://example.test/vosu/");
    assert.equal(options.fpsLimit, 144);
    assert.deepEqual(options.pids, [42, 99]);
    assert.deepEqual(options.processNames, ["custom.exe"]);
    assert.deepEqual(parseOptions([]).processNames, ["osu!.exe", "osulazer.exe"]);
    for (const argv of [
        ["--fps-limit", "0"],
        ["--fps-limit=241"],
        ["--pid=-1"],
        ["--vosu-url", "http://u:p@host/"],
        ["--vosu-url", "http://host/?x=1"],
        ["--vosu-url", "/bad"],
        ["stray"],
    ]) {
        assert.throws(() => parseOptions(argv));
    }
});

test("Vulkan launch parsing separates vosulay and game arguments", () => {
    const options = parseOptions([
        "--launch-vulkan",
        "/games/osu.AppImage",
        "--",
        "--no-audio",
        "--renderer=vulkan",
    ]);
    assert.deepEqual(options.vulkanLaunch, {
        args: ["--no-audio", "--renderer=vulkan"],
        executable: "/games/osu.AppImage",
    });
    assert.throws(
        () => parseOptions(["--launch-vulkan", "game", "--launch-vulkan", "other"]),
        /exactly one executable/,
    );
    assert.throws(() => parseOptions(["--", "game"]), /only valid after --launch-vulkan/);
});

test("visibility IPC invocation accepts both command aliases", () => {
    assert.equal(parseInvocation(["--ipc-toggle-overlay"]).command, "toggle-overlay");
    assert.equal(parseInvocation(["-i"]).command, "toggle-overlay");
});

test("invocation ignores Electron-injected Linux runtime switches", () => {
    const invocation = parseInvocation(
        [
            "--ozone-platform=wayland",
            "--disable-gpu-video-accelerated-decoding",
            ".",
            "--pid",
            "42",
        ],
        process.cwd(),
    );
    assert.deepEqual(invocation.options.pids, [42]);
});

test("overlay target selection ignores helpers and favors the largest client", () => {
    const helper = {
            height: 1,
            hwnd: 10,
            luid: { high: 0, low: 1 },
            width: 1,
        },
        main = {
            height: 720,
            hwnd: 20,
            luid: { high: 0, low: 2 },
            width: 1280,
        },
        secondary = {
            height: 480,
            hwnd: 30,
            luid: { high: 0, low: 3 },
            width: 640,
        };

    assert.equal(chooseOverlayTarget([helper, secondary, main]), main);
    assert.equal(chooseOverlayTarget([main, secondary, helper]), main);
    assert.equal(chooseOverlayTarget([helper]), undefined);
});

test("edit shortcuts map exact non-repeat Ctrl keydowns", () => {
    const input = {
        alt: false,
        control: true,
        isAutoRepeat: false,
        key: "i",
        meta: false,
        shift: false,
        type: "keyDown",
    };

    assert.equal(editShortcutCommand(input), "toggle-interaction");
    assert.equal(editShortcutCommand({ ...input, key: "r" }), "refresh-selected-overlay");
});

test("edit shortcuts reject keyUp, repeats, modifier drift, and wrong keys", () => {
    for (const key of ["i", "r"]) {
        const input = {
                alt: false,
                control: true,
                isAutoRepeat: false,
                key,
                meta: false,
                shift: false,
                type: "keyDown",
            },
            rejectedInputs = [
                { ...input, type: "keyUp" },
                { ...input, isAutoRepeat: true },
                { ...input, control: false },
                { ...input, alt: true },
                { ...input, meta: true },
                { ...input, shift: true },
                { ...input, key: "x" },
            ];

        for (const rejected of rejectedInputs) {
            assert.equal(editShortcutCommand(rejected), undefined);
        }
    }
});

test("native edit shortcuts follow Ctrl state and suppress held repeats", () => {
    const shortcuts = new NativeEditShortcutState(),
        key = (code: number, state: "Pressed" | "Released") => ({
            key: { code },
            kind: "Key",
            state,
        });

    assert.equal(shortcuts.handle(key(0x11, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x49, "Pressed")), "toggle-interaction");
    assert.equal(shortcuts.handle(key(0x49, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x49, "Released")), undefined);
    assert.equal(shortcuts.handle(key(0x52, "Pressed")), "refresh-selected-overlay");
});

test("native edit shortcuts reject extra modifiers and reset cleanly", () => {
    const shortcuts = new NativeEditShortcutState(),
        key = (code: number, state: "Pressed" | "Released") => ({
            key: { code },
            kind: "Key",
            state,
        });

    assert.equal(shortcuts.handle({ kind: "Char" }), undefined);
    assert.equal(shortcuts.handle(key(0xA2, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x10, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x49, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x49, "Released")), undefined);
    assert.equal(shortcuts.handle(key(0x10, "Released")), undefined);
    assert.equal(shortcuts.handle(key(0x49, "Pressed")), "toggle-interaction");
    assert.equal(shortcuts.handle(key(0x49, "Released")), undefined);
    assert.equal(shortcuts.handle(key(0x5D, "Pressed")), undefined);
    assert.equal(shortcuts.handle(key(0x52, "Pressed")), undefined);

    shortcuts.clear();
    assert.equal(shortcuts.handle(key(0x52, "Pressed")), undefined);
});

test("fatal deadline terminates a hung native request", async () => {
    let operationStarted = false;
    await assert.rejects(
        withFatalDeadline(
            () => {
                operationStarted = true;
                return new Promise<void>(() => {});
            },
            10,
            "fatal input deadline",
            (message): never => {
                throw new Error(message);
            },
        ),
        /fatal input deadline/,
    );
    assert.equal(operationStarted, true);
});

test("pending saves and flush acknowledgments bypass blocked editing dispatch", async () => {
    type Listener = (event: { sender: { id: number } }, payload: unknown) => void;
    let listener: Listener | undefined;
    const ipcMain = {
            on: (_channel: string, value: Listener): void => {
                listener = value;
            },
            removeListener: (_channel: string, value: Listener): void => {
                if (listener === value) {
                    listener = undefined;
                }
            },
        },
        sent: MessageEnvelope[] = [];
    let editingFinished = false,
        markEditingStarted!: () => void,
        markSaved!: () => void,
        releaseEditing!: () => void;
    const editingStarted = new Promise<void>((resolve) => {
            markEditingStarted = resolve;
        }),
        editingGate = new Promise<void>((resolve) => {
            releaseEditing = resolve;
        }),
        saved = new Promise<void>((resolve) => {
            markSaved = resolve;
        }),
        config = normalizeConfig({}),
        bridge = new HostBridge(
            {
                id: 7,
                isDestroyed: () => false,
                send: (_channel, envelope): void => {
                    sent.push(envelope);
                    if (envelope.type === "saved") {
                        markSaved();
                    }
                },
            },
            {
                editing: true,
                setEditing: async () => {
                    markEditingStarted();
                    await editingGate;
                    editingFinished = true;
                },
                toggleEditing: async () => {},
            },
            parseOptions([]),
            {
                save: async () => ({ config, revision: 1 }),
                subscribe: () => () => {},
            } as unknown as ConfigStore,
            { authorizeUrls: () => true } as unknown as LocalOverlayRegistry,
            ipcMain,
        );

    bridge.start();
    assert.ok(listener);
    listener({ sender: { id: 7 } }, { editing: false, type: "set-editing" });
    await editingStarted;

    const flush = bridge.requestSaveFlush(100),
        request = sent.find((message) => message.type === "flush-save");
    assert.equal(typeof request?.request_id, "string");
    listener(
        { sender: { id: 7 } },
        {
            base_revision: 0,
            config,
            save_id: "save-during-flush",
            type: "save-config",
        },
    );
    await saved;
    assert.equal(editingFinished, false);

    listener(
        { sender: { id: 7 } },
        {
            request_id: request?.request_id,
            type: "flush-complete",
        },
    );
    await flush;
    assert.equal(editingFinished, false);

    releaseEditing();
    await new Promise<void>((resolve) => setImmediate(resolve));
    assert.equal(editingFinished, true);
    bridge.dispose();
});

test("malformed config is normalized and saves serialize atomically", async () => {
    const normalized = normalizeConfig({
        active_profile: "missing",
        profiles: [
            {
                id: "same",
                name: " ",
                overlays: [
                    {
                        height: undefined,
                        opacity: 4,
                        rotation: -900,
                        scale: 0,
                        url: "file:///bad",
                        width: -2,
                        z_index: 1200,
                    },
                ],
            },
            { id: "same", name: "Second", overlays: "bad" },
        ],
        version: 99,
    });
    assert.equal(normalized.version, 1);
    assert.equal(normalized.active_profile, "same");
    assert.deepEqual(
        normalized.profiles.map((profile) => profile.id),
        ["same", "profile-2"],
    );
    assert.deepEqual(normalized.profiles[0].overlays[0], {
        counter_id: "",
        height: 300,
        left: 0,
        opacity: 1,
        rotation: -180,
        scale: 0.1,
        top: 0,
        width: 10,
        z_index: 999,
    });

    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-")),
        path = join(directory, "nested", "config.json"),
        store = new ConfigStore(path);
    await Promise.all([
        store.save({ active_profile: "one", profiles: [{ id: "one", name: "One", overlays: [] }] }),
        store.save({ active_profile: "two", profiles: [{ id: "two", name: "Two", overlays: [] }] }),
    ]);
    const persistedDefaults = JSON.parse(await readFile(path, "utf8")) as {
        active_profile: string;
        settings: AppSettings;
    };
    assert.equal(persistedDefaults.active_profile, "two");
    assert.match(persistedDefaults.settings.remote_access_token, /^[0-9a-f]{64}$/);
    assert.deepEqual(
        redactedRemoteAccessToken(persistedDefaults.settings),
        redactedRemoteAccessToken(normalizeConfig({}).settings),
    );
    await writeFile(path, "{bad", "utf8");
    const loaded = await store.load();
    assert.equal(loaded.config.active_profile, "default");
    assert.match(loaded.error ?? "", /Could not load configuration/);
});

test("config store persists a stable remote access credential on first load", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-credential-")),
        path = join(directory, "config.json"),
        store = new ConfigStore(path),
        first = await store.load(),
        second = await store.load(),
        persisted = JSON.parse(await readFile(path, "utf8")) as { settings: AppSettings };

    assert.match(first.config.settings.remote_access_token, /^[0-9a-f]{64}$/);
    assert.equal(
        second.config.settings.remote_access_token,
        first.config.settings.remote_access_token,
    );
    assert.equal(persisted.settings.remote_access_token, first.config.settings.remote_access_token);
});
test("config settings default and normalize manager hotkeys", () => {
    const defaults = normalizeConfig({}).settings;
    assert.match(defaults.remote_access_token, /^[0-9a-f]{64}$/);
    assert.deepEqual(redactedRemoteAccessToken(defaults), {
        allow_unrestricted_shared_array_buffer: false,
        allowed_ips: "127.0.0.1,localhost,absolute",
        always_keep_overlays_in_view: false,
        hotkeys: {
            copy_overlay: "Ctrl+C",
            paste_overlay: "Ctrl+V",
            profile_0: "Ctrl+0",
            profile_1: "Ctrl+1",
            profile_2: "Ctrl+2",
            profile_3: "Ctrl+3",
            profile_4: "Ctrl+4",
            profile_5: "Ctrl+5",
            profile_6: "Ctrl+6",
            profile_7: "Ctrl+7",
            profile_8: "Ctrl+8",
            profile_9: "Ctrl+9",
        },
        remote_access_host: "",
        remote_access_token: "[credential]",
    });

    assert.equal(
        normalizeConfig({ settings: { remote_access_host: " Gaming-PC.Local. " } }).settings
            .remote_access_host,
        "gaming-pc.local",
    );
    assert.equal(
        normalizeConfig({ settings: { remote_access_host: "https://attacker.example/path" } })
            .settings.remote_access_host,
        "",
    );

    const normalized = normalizeConfig({
        settings: {
            allow_unrestricted_shared_array_buffer: true,
            allowed_ips: "10.20.*.* 2001:db8::4",
            always_keep_overlays_in_view: true,
            hotkeys: {
                copy_overlay: "cmd+y",
                paste_overlay: " Control + option + spacebar ",
                profile_0: " option + shift + k ",
                profile_1: "f12",
                profile_2: "F13",
                profile_3: "Ctrl+K+Q",
                profile_4: 4,
            },
            remote_access_token: TEST_REMOTE_ACCESS_TOKEN,
            right_click_overlay_settings_menu: true,
        },
    });
    assert.deepEqual(normalized.settings, {
        allow_unrestricted_shared_array_buffer: true,
        allowed_ips: "10.20.*.* 2001:db8::4",
        always_keep_overlays_in_view: true,
        hotkeys: {
            copy_overlay: "Meta+Y",
            paste_overlay: "Ctrl+Alt+Space",
            profile_0: "Alt+Shift+K",
            profile_1: "F12",
            profile_2: "Ctrl+2",
            profile_3: "Ctrl+3",
            profile_4: "Ctrl+4",
            profile_5: "Ctrl+5",
            profile_6: "Ctrl+6",
            profile_7: "Ctrl+7",
            profile_8: "Ctrl+8",
            profile_9: "Ctrl+9",
        },
        remote_access_host: "",
        remote_access_token: TEST_REMOTE_ACCESS_TOKEN,
    });
});

test("config normalization rejects reserved shortcuts per action", () => {
    const reservedValues = [
        "control+i",
        " CTRL + r ",
        "control + shift + spacebar",
        "control+plus",
        " CTRL + shift + plus ",
        "control+minus",
    ];
    for (const reserved of reservedValues) {
        const { hotkeys } = normalizeConfig({
            settings: {
                hotkeys: {
                    copy_overlay: reserved,
                    paste_overlay: reserved,
                    profile_0: reserved,
                },
            },
        }).settings;
        assert.equal(hotkeys.profile_0, "Ctrl+0");
        assert.equal(hotkeys.copy_overlay, "Ctrl+C");
        assert.equal(hotkeys.paste_overlay, "Ctrl+V");
    }
});

test("config store persists manager settings without changing profiles", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-settings-")),
        path = join(directory, "config.json"),
        store = new ConfigStore(path),
        saved = await store.save({
            active_profile: "custom",
            profiles: [{ id: "custom", name: "Custom", overlays: [] }],
            settings: {
                allow_unrestricted_shared_array_buffer: true,
                always_keep_overlays_in_view: true,
                hotkeys: {
                    copy_overlay: "Meta+Y",
                    paste_overlay: "Ctrl+Alt+Space",
                    profile_0: "Alt+Shift+K",
                },
                right_click_overlay_settings_menu: true,
            },
        }),
        persisted = JSON.parse(await readFile(path, "utf8")) as unknown;
    assert.deepEqual(persisted, saved.config);
    assert.equal(Object.hasOwn(saved.config.settings, "right_click_overlay_settings_menu"), false);
    assert.ok(persisted && typeof persisted === "object" && "settings" in persisted);
    assert.ok(persisted.settings && typeof persisted.settings === "object");
    assert.equal(Object.hasOwn(persisted.settings, "right_click_overlay_settings_menu"), false);
    assert.equal(saved.config.active_profile, "custom");
    assert.deepEqual(saved.config.profiles, [{ id: "custom", name: "Custom", overlays: [] }]);
    assert.deepEqual((await store.load()).config, saved.config);
});

test("malformed config keeps one fallback credential until repaired", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-malformed-")),
        path = join(directory, "config.json");
    await writeFile(path, "{not json", "utf8");
    const store = new ConfigStore(path),
        first = await store.load(),
        second = await store.load();

    assert.match(first.error ?? "", /Could not load configuration/);
    assert.equal(
        second.config.settings.remote_access_token,
        first.config.settings.remote_access_token,
    );
});

test("config subscriptions carry revisions and reject stale saves", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-revisions-")),
        path = join(directory, "config.json"),
        store = new ConfigStore(path),
        firstSource = Symbol("first"),
        secondSource = Symbol("second"),
        changes: { revision: number; source: symbol | undefined; activeProfile: string }[] = [],
        unsubscribe = store.subscribe((change) => {
            changes.push({
                activeProfile: change.config.active_profile,
                revision: change.revision,
                source: change.source,
            });
        }),
        first = await store.save(
            { active_profile: "one", profiles: [{ id: "one", name: "One", overlays: [] }] },
            { expectedRevision: 0, source: firstSource },
        );
    assert.equal(first.revision, 1);
    await assert.rejects(
        store.save(
            { active_profile: "stale", profiles: [{ id: "stale", name: "Stale", overlays: [] }] },
            { expectedRevision: 0, source: secondSource },
        ),
        /changed since it was loaded/,
    );
    const second = await store.save(
            { active_profile: "two", profiles: [{ id: "two", name: "Two", overlays: [] }] },
            { expectedRevision: 1, source: secondSource },
        ),
        pending = store.save(
            { active_profile: "three", profiles: [{ id: "three", name: "Three", overlays: [] }] },
            { expectedRevision: 2, source: firstSource },
        );
    await store.flush();
    unsubscribe();

    assert.equal(second.revision, 2);
    assert.equal((await pending).revision, 3);
    assert.deepEqual(changes, [
        { activeProfile: "one", revision: 1, source: firstSource },
        { activeProfile: "two", revision: 2, source: secondSource },
        { activeProfile: "three", revision: 3, source: firstSource },
    ]);
    assert.equal(JSON.parse(await readFile(path, "utf8")).active_profile, "three");
});

test("config flush reports the latest persistence failure", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-config-failure-")),
        blockingFile = join(directory, "not-a-directory");
    await writeFile(blockingFile, "block", "utf8");
    const store = new ConfigStore(join(blockingFile, "config.json"));
    await assert.rejects(store.save({ profiles: [] }));
    await assert.rejects(store.flush());
});

test("catalog parser decodes escaped metadata and preserves source order", () => {
    const source = `
            <article class="counter-card" data-overlay-id="folder/&amp;&quot;"
                     data-counter-name="First &amp; Best"
                     data-counter-author="A &quot;Name&quot;"
                     data-counter-version="1.2">
              <header>
                <button type="button" data-action="counter-settings">Settings</button>
              </header>
              <div class="counter-card__preview">
                <iframe src="/folder/" width="640px" height="360px"></iframe>
              </div>
            </article>
            <article class="counter-card is-pinned" data-overlay-id="second"
                     data-counter-name="Second"
                     data-counter-author="Author"
                     data-counter-version="2">
              <div class="counter-card__preview">
                <iframe src="/second/" width="800" height="600"></iframe>
              </div>
            </article>
        `,
        counters = parseInstalledCounters("http://127.0.0.1:24050/base?old=1#fragment", source);

    assert.deepEqual(counters, [
        {
            author: 'A "Name"',
            has_settings: true,
            height: 360,
            id: 'folder/&"',
            name: "First & Best",
            url: "http://127.0.0.1:24050/folder%2F&%22/",
            version: "1.2",
            width: 640,
        },
        {
            author: "Author",
            has_settings: false,
            height: 600,
            id: "second",
            name: "Second",
            url: "http://127.0.0.1:24050/second/",
            version: "2",
            width: 800,
        },
    ]);
});

test("catalog parser accepts legacy Tosu counter markup", () => {
    const source = `
            <div class="result-item downloaded">
              <div class="ri-head flexer">
                <div>
                  <h4 class="flexer">Legacy Counter v2.4</h4>
                  <div class="ri-links flexer"><span>by Legacy Author</span></div>
                </div>
                <div class="calu buttons-group flexer" n="Legacy Counter" a="Legacy Author" v="2.4">
                  <button class="button settings-builder-button flexer" n="legacy/folder"></button>
                  <button class="button settings-button flexer" n="legacy/folder"></button>
                  <button class="button open-button flexer" n="legacy/folder"></button>
                  <button class="button delete-button flexer" n="legacy/folder"></button>
                </div>
              </div>
              <div class="ri-gallery flexer">
                <iframe src="/legacy/folder/" width="720px" height="480px"></iframe>
              </div>
            </div>
            <div class="result-item">
              <div class="calu buttons-group flexer" n="Second Legacy" a="Author" v="1.0">
                <button class="button settings-builder-button flexer" n="second"></button>
                <button class="button open-button flexer" n="second"></button>
                <button class="button delete-button flexer" n="second"></button>
              </div>
              <div class="ri-gallery flexer"><img src="preview.png"></div>
            </div>
        `,
        counters = parseInstalledCounters("http://127.0.0.1:24050", source);

    assert.deepEqual(counters, [
        {
            author: "Legacy Author",
            has_settings: true,
            height: 480,
            id: "legacy/folder",
            name: "Legacy Counter",
            url: "http://127.0.0.1:24050/legacy%2Ffolder/",
            version: "2.4",
            width: 720,
        },
        {
            author: "Author",
            has_settings: false,
            height: 300,
            id: "second",
            name: "Second Legacy",
            url: "http://127.0.0.1:24050/second/",
            version: "1.0",
            width: 500,
        },
    ]);
});

test("catalog parser defaults flexible, non-numeric, and invalid resolutions", () => {
    const source = `
            <article class="counter-card" data-overlay-id="flexible"
                     data-counter-name="Flexible"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/flexible/" width="100%" height="300px"></iframe>
              </div>
            </article>
            <article class="counter-card" data-overlay-id="non-numeric"
                     data-counter-name="Non-numeric"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/non-numeric/" width="ANY" height="auto"></iframe>
              </div>
            </article>
            <article class="counter-card" data-overlay-id="too-small"
                     data-counter-name="Too small"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/too-small/" width="10px" height="300px"></iframe>
              </div>
            </article>
        `,
        counters = parseInstalledCounters("http://127.0.0.1:24050", source);

    assert.deepEqual(
        counters.map(({ width, height }) => [width, height]),
        [
            [500, 300],
            [500, 300],
            [500, 300],
        ],
    );
});

test("catalog parser rejects a missing counter attribute", () => {
    const source = `
            <article class="counter-card" data-overlay-id="missing-version"
                     data-counter-name="Missing version"
                     data-counter-author="Author">
              <div class="counter-card__preview">
                <iframe src="/missing-version/" width="640px" height="360px"></iframe>
              </div>
            </article>
        `;

    assert.throws(
        () => parseInstalledCounters("http://127.0.0.1:24050", source),
        /"missing-version" is missing the "data-counter-version" attribute/,
    );
});

test("catalog parser accepts modern and legacy empty states", () => {
    for (const source of [
        '<div class="empty-state">No counters found.</div>',
        '<div class="no-results">No counters<br />Change your search phrase</div>',
    ]) {
        assert.deepEqual(parseInstalledCounters("http://127.0.0.1:24050", source), []);
    }
});

test("catalog parser rejects unrecognized markup", () => {
    assert.throws(
        () => parseInstalledCounters("http://127.0.0.1:24050", "<main>Unexpected response</main>"),
        /counter search markup was not recognized; expected modern Vosu or legacy Tosu results/,
    );
});

test("catalog fetch validates type and enforces the streaming limit", async () => {
    const html = `<article class="counter-card" data-overlay-id="x"
        data-counter-name="X" data-counter-author="A" data-counter-version="1"></article>`,
        fetcher = async () =>
            new Response(html, { headers: { "content-type": "text/html; charset=utf-8" } });
    assert.equal((await fetchInstalledCounters("http://host", fetcher as typeof fetch))[0].id, "x");
    const oversized = async () =>
        new Response(new Uint8Array(MAX_COUNTER_PAGE_BYTES + 1), {
            headers: { "content-type": "application/xhtml+xml" },
        });
    await assert.rejects(
        fetchInstalledCounters("http://host", oversized as typeof fetch),
        /byte limit/,
    );
    const wrongType = async () =>
        new Response("{}", { headers: { "content-type": "application/json" } });
    await assert.rejects(
        fetchInstalledCounters("http://host", wrongType as typeof fetch),
        /non-HTML/,
    );
});

test("URL helpers encode identifiers and replace base paths", () => {
    assert.equal(counterUrl("http://host/root/", "a/b &").href, "http://host/a%2Fb%20&/");
    assert.equal(
        settingsUrl("http://host/root/", "a&b").href,
        "http://host/settings?overlay=a%26b",
    );
    assert.equal(availableUrl("http://host/root/").href, "http://host/available");
});

test("manager source embedding cannot terminate its bootstrap script", () => {
    const document = buildManagerDocumentFromSources(
        "<script>__VOSULAY_BOOTSTRAP__</script>",
        "</style><script>x</script>",
        "</script>\u2028",
        120,
    );
    assert.match(document, /__VOSULAY_FPS_LIMIT__=120/);
    assert.ok(!document.includes("</style><script>x</script>"));
    assert.equal(document.match(/<\/script>/g)?.length, 1);
    assert.ok(!document.includes("__VOSULAY_BOOTSTRAP__"));
});

test("local overlay authorization handles scheme case and cache-busted assets while rejecting escapes", async () => {
    const directory = await mkdtemp(join(tmpdir(), "vosulay-overlay-")),
        root = join(directory, "root"),
        other = join(directory, "other");
    await mkdir(root);
    await mkdir(other);
    await writeFile(join(root, "index.html"), "ok");
    await writeFile(join(root, "script.js"), "window.overlayLoaded = true;");
    await writeFile(join(other, "secret.html"), "secret");
    await symlink(join(other, "secret.html"), join(root, "escape.html"));
    const canonicalRoot = await realpath(root),
        capabilityKey = randomBytes(32),
        registry = new LocalOverlayRegistry(capabilityKey),
        url = await registry.createUrl(join(root, "index.html"));
    assert.equal(await registry.resolveAuthorizedFile(url), join(canonicalRoot, "index.html"));
    const uppercaseUrl = url.replace(/^vosulay-overlay:/, "VOSULAY-OVERLAY:");
    assert.deepEqual(localOverlayUrls({ overlays: [{ url: uppercaseUrl }] }), [uppercaseUrl]);
    const importedRegistry = new LocalOverlayRegistry(capabilityKey);
    assert.equal(
        importedRegistry.authorizeUrls(localOverlayUrls({ overlays: [{ url: uppercaseUrl }] })),
        true,
    );
    const assetUrl = new URL(url);
    assetUrl.pathname = `${assetUrl.pathname.slice(0, assetUrl.pathname.lastIndexOf("/") + 1)}script.js`;
    assetUrl.search = "?v=1";
    assetUrl.hash = "#loaded";
    assert.equal(
        await importedRegistry.resolveAuthorizedFile(assetUrl),
        join(canonicalRoot, "script.js"),
    );
    const response = await importedRegistry.response(new Request(assetUrl));
    assert.equal(response.status, 200);
    assert.equal(await response.text(), "window.overlayLoaded = true;");
    const parsed = new URL(url),
        rebound = new URL(url);
    rebound.pathname = `/${Buffer.from(other).toString("base64url")}/secret.html`;
    assert.equal(registry.authorizeUrl(rebound), false);
    const traversal = new URL(url);
    traversal.pathname = `${parsed.pathname.slice(0, parsed.pathname.lastIndexOf("/") + 1)}%2e%2e%2fsecret.html`;
    await assert.rejects(registry.resolveAuthorizedFile(traversal));
    const escaped = new URL(url);
    escaped.pathname = `${parsed.pathname.slice(0, parsed.pathname.lastIndexOf("/") + 1)}escape.html`;
    await assert.rejects(registry.resolveAuthorizedFile(escaped), /escapes its root/);
});
