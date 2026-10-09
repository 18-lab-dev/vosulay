(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const stage = $("stage");
  const editor = $("editor");
  const inspector = $("inspector");
  const toolbar = editor.querySelector(".toolbar");
  const sidePanels = document.querySelectorAll(".sidebar, #inspector");
  const sidePanelClearance = 24;
  const profileSelect = $("profile-select");
  const profileSelectLabel = $("profile-select-label");
  const profileMenu = $("profile-menu");
  const profilePicker = profileSelect.closest(".profile-picker");
  const profileAdd = $("profile-add");
  const profileDelete = $("profile-delete");
  const profileActions = $("profile-actions");
  const profilePosition = $("profile-position");
  const profilePositionTotal = $("profile-position-total");
  const profileImport = $("profile-import");
  const profileExport = $("profile-export");
  const profileDeleteConfirm = $("profile-delete-confirm");
  const profileDeleteMessage = $("profile-delete-message");
  const profileDeleteAccept = $("profile-delete-accept");
  const profileDeleteCancel = $("profile-delete-cancel");
  const profileCreate = $("profile-create");
  const profileName = $("profile-name");
  const profileCreateCancel = $("profile-create-cancel");
  const catalog = $("catalog");
  const status = $("status");
  const undoButton = $("undo");
  const redoButton = $("redo");
  const refreshSelectedButton = $("refresh-selected-overlays");
  const refreshAllButton = $("refresh-all-overlays");
  const addUrl = $("add-url");
  const urlModal = $("url-modal");
  const urlForm = $("url-form");
  const urlName = $("url-name");
  const urlInput = $("url-input");
  const urlCancel = $("url-cancel");
  const nameField = $("field-name");
  const urlField = $("field-url");
  const urlFieldLabel = $("field-url-label");
  const fields = {
    left: $("field-left"), top: $("field-top"), width: $("field-width"),
    height: $("field-height"), scale: $("field-scale"), z_index: $("field-z"),
    opacity: $("field-opacity"), rotation: $("field-rotation")
  };
  const enabledField = $("field-enabled");
  enabledField.addEventListener("change", () => {
    const overlay = selectedOverlay();
    if (!overlay) return;
    const before = historySnapshot();
    overlay.enabled = enabledField.checked;
    commitHistory(before);
    scheduleSave(true);
    requestRender("stageStructure", "inspector");
  });
  const openSettings = $("open-settings");
  const settingsModal = $("settings-modal");
  const settingsForm = $("settings-form");
  const keepOverlaysInView = $("keep-overlays-in-view");
  const fpsLimitModes = settingsForm.querySelectorAll('input[name="fps-limit-mode"]');
  const fpsLimit = $("fps-limit");
  const allowUnrestrictedSharedArrayBuffer = $("allow-unrestricted-shared-array-buffer");
  const allowedIps = $("allowed-ips");
  const remoteListenHost = $("remote-listen-host");
  const remoteListenPort = $("remote-listen-port");
  const hotkeyList = $("hotkey-list");
  const settingsError = $("settings-error");
  const settingsResetHotkeys = $("settings-reset-hotkeys");
  const settingsCancel = $("settings-cancel");
  const selectionBox = $("selection-box");
  const HOTKEY_KEYS = [
    ...Array.from({ length: 10 }, (_, index) => `profile_${index}`),
    "copy_overlay",
    "paste_overlay",
    "undo",
    "redo",
    "refresh_selected_overlay",
    "refresh_all_overlays"
  ];
  const REPEATABLE_HOTKEYS = new Set(["undo", "redo"]);
  const DEFAULT_ALLOWED_IPS = "127.0.0.1,localhost,absolute";
  const DEFAULT_REMOTE_LISTEN_HOST = "127.0.0.1";
  const DEFAULT_HOTKEYS = Object.freeze({
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
    copy_overlay: "Ctrl+C",
    paste_overlay: "Ctrl+V",
    undo: "Ctrl+Z",
    redo: "Ctrl+Shift+Z",
    refresh_selected_overlay: "Ctrl+R",
    refresh_all_overlays: "Ctrl+Shift+R"
  });
  // Ctrl+I is consumed natively to toggle overlay interaction; the rest are
  // browser zoom and the overlay toggle chord, which never reach page hotkeys.
  const RESERVED_HOTKEYS = new Set([
    "Ctrl+I", "Ctrl+Shift+Space",
    "Ctrl+Plus", "Ctrl+Shift+Plus", "Ctrl+Minus"
  ]);
  const HISTORY_LIMIT = 100;
  const HISTORY_COALESCE_MS = 1000;

  const dirty = {
    stageGeometry: true,
    stageStructure: true,
    profileList: true,
    catalog: true,
    inspector: true,
    chrome: true
  };
  const counterById = new Map();
  const overlayRoots = new Map();
  const state = {
    config: null,
    counters: null,
    editing: false,
    interacting: false,
    selected: new Set(),
    copiedPlacements: [],
    settingsDraft: null,
    recordingHotkey: "",
    inspectorPopup: false,
    inspectorPoint: null,
    inspectorCycle: null,
    saveTimer: 0,
    saveRevision: 0,
    latestSaveId: "",
    pendingSaveId: "",
    hostRevision: null,
    saveInFlightId: "",
    deferredConfig: null,
    flushRequestIds: [],
    saveErrorMessage: "",
    retryRequired: false,
    renderPending: false,
    pointer: null,
    resizeFrame: 0,
    toolbarHold: false,
    inspectorEdit: null,
    error: null
  };

  const outboundQueue = [];
  if (typeof window.__VOSULAY_POST_MESSAGE__ !== "function") {
    window.__VOSULAY_POST_MESSAGE__ = (payload) => {
      const encoded = typeof payload === "string" ? payload : JSON.stringify(payload);
      document.documentElement.setAttribute("data-vosulay-bridge-message", encoded);
      document.documentElement.dispatchEvent(new Event("vosulay-bridge-message", { bubbles: true }));
      document.documentElement.removeAttribute("data-vosulay-bridge-message");
    };
    window.ipc = { postMessage: (payload) => window.__VOSULAY_POST_MESSAGE__(payload) };
  }


  function invokeBridge(payload) {
    const bridge = window.__VOSULAY_POST_MESSAGE__;
    if (typeof bridge !== "function") return false;
    try {
      bridge(payload);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    }
    return true;
  }

  function flushOutboundQueue() {
    while (outboundQueue.length && typeof window.__VOSULAY_POST_MESSAGE__ === "function") {
      invokeBridge(outboundQueue.shift());
    }
  }

  window.addEventListener("vosulay-bridge-ready", flushOutboundQueue);

  function ipc(message) {
    let payload;
    try {
      payload = JSON.stringify(message);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
      return;
    }
    if (!invokeBridge(payload)) outboundQueue.push(payload);
  }

  function finite(value, fallback) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function clamp(value, minimum, maximum) {
    return Math.min(maximum, Math.max(minimum, value));
  }

  function safeId(value, fallback) {
    const id = typeof value === "string" ? value.trim() : "";
    return id || fallback;
  }

  function normalizeOverlay(input) {
    const overlay = input && typeof input === "object" ? input : {};
    const width = Math.max(10, finite(overlay.width, 500));
    const height = Math.max(10, finite(overlay.height, 300));
    const scale = Math.max(0.1, finite(overlay.scale, 1));
    const opacity = clamp(finite(overlay.opacity, 1), 0, 1);
    const rotation = clamp(finite(overlay.rotation, 0), -180, 180);
    const normalized = {
      counter_id: safeId(overlay.counter_id, "missing"),
      enabled: overlay.enabled !== false,
      top: finite(overlay.top, 0),
      left: finite(overlay.left, 0),
      width,
      height,
      scale,
      opacity,
      rotation,
      z_index: Math.round(clamp(finite(overlay.z_index, 0), 0, 999))
    };
    if (typeof overlay.url === "string" && overlay.url.trim()) normalized.url = overlay.url.trim();
    if (typeof overlay.name === "string" && overlay.name.trim()) normalized.name = overlay.name.trim();
    return normalized;
  }

  function parseSource(value, protocols) {
    if (typeof value !== "string" || !value.trim()) return null;
    try {
      const url = new URL(value.trim());
      return protocols.includes(url.protocol) ? url : null;
    } catch (_) {
      return null;
    }
  }

  function customSource(overlay) {
    return overlay && overlay.url ? parseSource(overlay.url, ["http:", "https:", "vosulay-overlay:"]) : null;
  }

  function canonicalHotkey(value, fallback) {
    if (typeof value !== "string") return fallback;
    const rawParts = value.split("+").map((part) => part.trim()).filter(Boolean);
    if (!rawParts.length) return fallback;
    const aliases = {
      control: "Ctrl", ctrl: "Ctrl", alt: "Alt", option: "Alt",
      shift: "Shift", meta: "Meta", command: "Meta", cmd: "Meta",
      super: "Meta", win: "Meta", windows: "Meta"
    };
    const modifiers = new Set();
    let key = "";
    for (const part of rawParts) {
      const modifier = aliases[part.toLowerCase()];
      if (modifier) {
        modifiers.add(modifier);
      } else if (!key) {
        const lower = part.toLowerCase();
        const names = {
          esc: "Escape", escape: "Escape", space: "Space", spacebar: "Space",
          return: "Enter", enter: "Enter", del: "Delete", delete: "Delete",
          backspace: "Backspace", tab: "Tab", home: "Home", end: "End",
          pageup: "PageUp", pagedown: "PageDown", arrowup: "ArrowUp",
          arrowdown: "ArrowDown", arrowleft: "ArrowLeft", arrowright: "ArrowRight",
          plus: "Plus", minus: "Minus"
        };
        key = names[lower] || (/^[a-z0-9]$/i.test(part) ? part.toUpperCase()
          : (/^f(?:[1-9]|1[0-2])$/i.test(part) ? part.toUpperCase() : ""));
      } else {
        return fallback;
      }
    }
    if (!key) return fallback;
    return ["Ctrl", "Alt", "Shift", "Meta"].filter((modifier) => modifiers.has(modifier)).concat(key).join("+");
  }

  function normalizeHotkeys(input) {
    const source = input && typeof input === "object" ? input : {};
    return Object.fromEntries(HOTKEY_KEYS.map((key) => {
      const shortcut = canonicalHotkey(source[key], DEFAULT_HOTKEYS[key]);
      return [key, RESERVED_HOTKEYS.has(shortcut) ? DEFAULT_HOTKEYS[key] : shortcut];
    }));
  }

  function normalizeConfig(input) {
    const source = input && typeof input === "object" ? input : {};
    let profiles = Array.isArray(source.profiles) ? source.profiles.map((profile, index) => ({
      id: safeId(profile && profile.id, `profile-${index + 1}`),
      name: safeId(profile && profile.name, `Profile ${index + 1}`),
      overlays: Array.isArray(profile && profile.overlays) ? profile.overlays.map(normalizeOverlay) : []
    })) : [];
    if (!profiles.length) profiles = [{ id: "default", name: "Default", overlays: [] }];
    const ids = new Set();
    profiles.forEach((profile, index) => {
      let id = profile.id;
      while (ids.has(id)) id = `${profile.id}-${index + 1}`;
      profile.id = id;
      ids.add(id);
    });
    const requested = safeId(source.active_profile, profiles[0].id);
    const sourceSettings = source.settings && typeof source.settings === "object" ? source.settings : {};
    return {
      version: 1,
      active_profile: ids.has(requested) ? requested : profiles[0].id,
      profiles,
      tosu_migration_completed: Boolean(source.tosu_migration_completed),
      settings: {
        fps_limit: Number.isInteger(sourceSettings.fps_limit)
          && sourceSettings.fps_limit >= 0 && sourceSettings.fps_limit <= 4294967295
          ? Math.min(sourceSettings.fps_limit, 1000000) : 0,
        always_keep_overlays_in_view: Boolean(sourceSettings.always_keep_overlays_in_view),
        allow_unrestricted_shared_array_buffer: Boolean(sourceSettings.allow_unrestricted_shared_array_buffer),
        allowed_ips: typeof sourceSettings.allowed_ips === "string" ? sourceSettings.allowed_ips : DEFAULT_ALLOWED_IPS,
        remote_listen_host: typeof sourceSettings.remote_listen_host === "string"
          ? sourceSettings.remote_listen_host : DEFAULT_REMOTE_LISTEN_HOST,
        remote_listen_port: Number.isInteger(sourceSettings.remote_listen_port)
          && sourceSettings.remote_listen_port >= 1 && sourceSettings.remote_listen_port <= 65535
          ? sourceSettings.remote_listen_port : DEFAULT_REMOTE_LISTEN_PORT,
        hotkeys: normalizeHotkeys(sourceSettings.hotkeys)
      }
    };
  }

  function activeProfile() {
    if (!state.config) return null;
    return state.config.profiles.find((profile) => profile.id === state.config.active_profile) || state.config.profiles[0] || null;
  }

  function findCounter(id) {
    return counterById.get(id) || null;
  }

  function selectedOverlay() {
    const profile = activeProfile();
    if (!profile || state.selected.size !== 1) return null;
    const overlay = state.selected.values().next().value;
    return profile.overlays.includes(overlay) ? overlay : null;
  }

  function overlayContainsPoint(overlay, x, y) {
    const scale = Math.max(0.0001, finite(overlay.scale, 1));
    const angle = finite(overlay.rotation, 0) * Math.PI / 180;
    const cosine = Math.cos(angle);
    const sine = Math.sin(angle);
    const deltaX = x - finite(overlay.left, 0);
    const deltaY = y - finite(overlay.top, 0);
    const localX = (deltaX * cosine + deltaY * sine) / scale;
    const localY = (-deltaX * sine + deltaY * cosine) / scale;
    return localX >= 0
      && localX <= finite(overlay.width, 0)
      && localY >= 0
      && localY <= finite(overlay.height, 0);
  }

  function overlaysAtPoint(x, y) {
    const profile = activeProfile();
    if (!profile) return [];
    return profile.overlays
      .map((overlay, index) => ({ overlay, index }))
      .filter(({ overlay }) => overlayRoots.has(overlay) && overlayContainsPoint(overlay, x, y))
      .sort((left, right) => (
        finite(right.overlay.z_index, 0) - finite(left.overlay.z_index, 0)
        || right.index - left.index
      ))
      .map(({ overlay }) => overlay);
  }

  function sameOverlayOrder(left, right) {
    return left.length === right.length && left.every((overlay, index) => overlay === right[index]);
  }

  function contextOverlay(overlays, clickedOverlay) {
    if (overlays.length < 2) {
      state.inspectorCycle = null;
      return clickedOverlay || overlays[0] || null;
    }

    const previous = state.inspectorCycle;
    let index = clickedOverlay ? overlays.indexOf(clickedOverlay) : -1;
    if (previous && sameOverlayOrder(previous.overlays, overlays)) {
      const previousIndex = overlays.indexOf(previous.overlay);
      if (previousIndex >= 0) index = (previousIndex + 1) % overlays.length;
    }
    if (index < 0) index = 0;
    const overlay = overlays[index];
    state.inspectorCycle = { overlays, overlay };
    return overlay;
  }

  function rectsNear(first, second, clearance) {
    return first.left <= second.right + clearance
      && first.right >= second.left - clearance
      && first.top <= second.bottom + clearance
      && first.bottom >= second.top - clearance;
  }

  function updatePanelAvoidance() {
    updateToolbarAvoidance();
    const overlay = state.editing ? selectedOverlay() : null;
    const root = overlay && overlayRoots.get(overlay);
    if (!root) {
      for (const panel of sidePanels) panel.classList.remove("avoids-selected-overlay");
      return;
    }
    const overlayRect = root.getBoundingClientRect();
    for (const panel of sidePanels) {
      if (panel === inspector && document.body.classList.contains("context-inspector")) {
        panel.classList.remove("avoids-selected-overlay");
        continue;
      }
      panel.classList.toggle("avoids-selected-overlay", rectsNear(overlayRect, panel.getBoundingClientRect(), sidePanelClearance));
    }
  }

  // The toolbar holds Done and every open menu, so it never hides while it is
  // in use; otherwise the user could be locked out of leaving the editor.
  function toolbarPinned() {
    return !profileMenu.hidden || !profileCreate.hidden || !profileDeleteConfirm.hidden
      || !settingsModal.hidden || !urlModal.hidden || toolbar.contains(document.activeElement);
  }

  function updateToolbarAvoidance() {
    const operation = state.pointer;
    if (!state.editing) {
      state.toolbarHold = false;
    } else if (operation && operation.kind === "overlay") {
      const toolbarRect = toolbar.getBoundingClientRect();
      state.toolbarHold = operation.placements.some(({ overlay }) => {
        const root = overlayRoots.get(overlay);
        return Boolean(root) && rectsNear(root.getBoundingClientRect(), toolbarRect, sidePanelClearance);
      });
    }
    toolbar.classList.toggle("avoids-selected-overlay", state.toolbarHold && !toolbarPinned());
  }

  // After a drag ends near the toolbar it stays hidden until the pointer leaves
  // the toolbar area, so releasing an overlay there does not cover it again.
  function releaseToolbarHold(x, y) {
    if (!state.toolbarHold || state.pointer) return;
    if (Number.isFinite(x) && Number.isFinite(y)) {
      const rect = toolbar.getBoundingClientRect();
      if (rectsNear({ left: x, right: x, top: y, bottom: y }, rect, sidePanelClearance)) return;
    }
    state.toolbarHold = false;
    toolbar.classList.remove("avoids-selected-overlay");
  }

  function transformedBounds(overlay) {
    const angle = overlay.rotation * Math.PI / 180;
    const cosine = Math.cos(angle);
    const sine = Math.sin(angle);
    const widthX = overlay.scale * overlay.width * cosine;
    const widthY = overlay.scale * overlay.width * sine;
    const heightX = -overlay.scale * overlay.height * sine;
    const heightY = overlay.scale * overlay.height * cosine;
    return {
      minX: Math.min(0, widthX, heightX, widthX + heightX),
      maxX: Math.max(0, widthX, heightX, widthX + heightX),
      minY: Math.min(0, widthY, heightY, widthY + heightY),
      maxY: Math.max(0, widthY, heightY, widthY + heightY)
    };
  }

  function overlayPositionRange(overlay) {
    if (!state.config?.settings.always_keep_overlays_in_view) {
      const angle = overlay.rotation * Math.PI / 180;
      const centerX = overlay.scale
        * (overlay.width * Math.cos(angle) - overlay.height * Math.sin(angle)) / 2;
      const centerY = overlay.scale
        * (overlay.width * Math.sin(angle) + overlay.height * Math.cos(angle)) / 2;
      return {
        minLeft: -centerX,
        maxLeft: window.innerWidth - centerX,
        minTop: -centerY,
        maxTop: window.innerHeight - centerY
      };
    }

    const bounds = transformedBounds(overlay);
    const boundWidth = bounds.maxX - bounds.minX;
    const boundHeight = bounds.maxY - bounds.minY;
    const centeredLeft = (window.innerWidth - boundWidth) / 2 - bounds.minX;
    const centeredTop = (window.innerHeight - boundHeight) / 2 - bounds.minY;
    return {
      minLeft: boundWidth > window.innerWidth ? centeredLeft : -bounds.minX,
      maxLeft: boundWidth > window.innerWidth ? centeredLeft : window.innerWidth - bounds.maxX,
      minTop: boundHeight > window.innerHeight ? centeredTop : -bounds.minY,
      maxTop: boundHeight > window.innerHeight ? centeredTop : window.innerHeight - bounds.maxY
    };
  }

  function clampOverlay(overlay) {
    // Only the sole selected placement is constrained: entering the editor,
    // display resizes, and settings changes must never rewrite placements the
    // user did not pick. Game/display changes outside the editor are ignored too.
    if (!state.editing || state.selected.size !== 1 || !state.selected.has(overlay)) return;
    overlay.scale = Math.max(0.1, finite(overlay.scale, 1));
    overlay.opacity = clamp(finite(overlay.opacity, 1), 0, 1);
    overlay.rotation = clamp(finite(overlay.rotation, 0), -180, 180);
    overlay.width = Math.max(10, finite(overlay.width, 500));
    overlay.height = Math.max(10, finite(overlay.height, 300));
    overlay.z_index = Math.round(clamp(finite(overlay.z_index, 0), 0, 999));

    const angle = overlay.rotation * Math.PI / 180;
    const cosine = Math.cos(angle);
    const sine = Math.sin(angle);
    if (!state.config?.settings.always_keep_overlays_in_view) {
      const centerX = overlay.scale * (overlay.width * cosine - overlay.height * sine) / 2;
      const centerY = overlay.scale * (overlay.width * sine + overlay.height * cosine) / 2;
      if (document.activeElement !== fields.left) {
        overlay.left = clamp(finite(overlay.left, 0), -centerX, window.innerWidth - centerX);
      }
      if (document.activeElement !== fields.top) {
        overlay.top = clamp(finite(overlay.top, 0), -centerY, window.innerHeight - centerY);
      }
      return;
    }

    const boundWidthAtScaleOne = Math.abs(cosine) * overlay.width + Math.abs(sine) * overlay.height;
    const boundHeightAtScaleOne = Math.abs(sine) * overlay.width + Math.abs(cosine) * overlay.height;
    const fittingScale = Math.min(
      boundWidthAtScaleOne ? window.innerWidth / boundWidthAtScaleOne : overlay.scale,
      boundHeightAtScaleOne ? window.innerHeight / boundHeightAtScaleOne : overlay.scale
    );
    if (overlay.scale > fittingScale) overlay.scale = Math.max(0.1, fittingScale);
    const range = overlayPositionRange(overlay);
    if (document.activeElement !== fields.left) {
      overlay.left = clamp(finite(overlay.left, 0), range.minLeft, range.maxLeft);
    }
    if (document.activeElement !== fields.top) {
      overlay.top = clamp(finite(overlay.top, 0), range.minTop, range.maxTop);
    }
  }

  function applyOverlayStyle(root, overlay) {
    const next = {
      left: `${overlay.left}px`,
      top: `${overlay.top}px`,
      width: `${overlay.width}px`,
      height: `${overlay.height}px`,
      transform: `rotate(${overlay.rotation}deg) scale(${overlay.scale})`,
      opacity: String(overlay.opacity),
      zIndex: String(overlay.z_index)
    };
    const applied = root._vosulayPlacement || (root._vosulayPlacement = {});
    if (applied.left !== next.left) root.style.left = next.left;
    if (applied.top !== next.top) root.style.top = next.top;
    if (applied.width !== next.width) root.style.width = next.width;
    if (applied.height !== next.height) root.style.height = next.height;
    if (applied.transform !== next.transform) root.style.transform = next.transform;
    if (applied.opacity !== next.opacity) root.style.setProperty("--overlay-opacity", next.opacity);
    if (applied.zIndex !== next.zIndex) root.style.zIndex = next.zIndex;
    root._vosulayPlacement = next;
  }

  function markDirty(...regions) {
    for (const region of regions) dirty[region] = true;
  }

  function requestRender(...regions) {
    markDirty(...regions);
    if (state.renderPending) return;
    state.renderPending = true;
    requestAnimationFrame(() => {
      state.renderPending = false;
      render();
    });
  }

  function flushPendingSave() {
    if (!state.pendingSaveId) return;
    if (Number.isSafeInteger(state.hostRevision) && state.saveInFlightId) return;
    const saveId = state.pendingSaveId;
    state.pendingSaveId = "";
    if (state.saveTimer) {
      clearTimeout(state.saveTimer);
      state.saveTimer = 0;
    }
    const message = { type: "save-config", config: state.config, save_id: saveId };
    if (Number.isSafeInteger(state.hostRevision)) {
      message.base_revision = state.hostRevision;
      state.saveInFlightId = saveId;
    }
    ipc(message);
  }

  function settleFlushRequests(error) {
    if (!error && (state.pendingSaveId || state.saveInFlightId)) return;
    const type = error ? "flush-failed" : "flush-complete";
    for (const requestId of state.flushRequestIds.splice(0)) {
      ipc({ type, request_id: requestId, ...(error ? { message: error } : {}) });
    }
  }

  function applyHostConfig(envelope) {
    state.retryRequired = false;
    replaceSelection([]);
    state.config = normalizeConfig(envelope.config !== undefined ? envelope.config : envelope.payload);
    // Snapshots of a replaced configuration would resurrect state the host
    // rejected or superseded (conflict recovery, tosu import, first load).
    resetHistory();
    if (Number.isSafeInteger(envelope.revision) && envelope.revision >= 0) state.hostRevision = envelope.revision;
    const invalidSource = state.config.profiles
      .flatMap((item) => item.overlays)
      .find((overlay) => overlay.url && !customSource(overlay));
    if (invalidSource) setError(`Unsupported custom overlay source: ${invalidSource.url}`);
    else {
      state.error = null;
      status.textContent = "";
    }
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  }
  function scheduleSave(immediate = false) {
    if (!state.config) return;
    state.retryRequired = false;
    const saveId = String(++state.saveRevision);
    state.latestSaveId = saveId;
    state.pendingSaveId = saveId;
    clearTimeout(state.saveTimer);
    status.textContent = "Saving…";
    if (immediate) flushPendingSave();
    else state.saveTimer = window.setTimeout(flushPendingSave, 240);
  }

  const history = { undo: [], redo: [], lastKey: "", lastAt: 0 };

  function historySnapshot() {
    if (!state.config) return null;
    const profile = activeProfile();
    return {
      profiles: JSON.stringify(state.config.profiles),
      active_profile: state.config.active_profile,
      selected: profile
        ? profile.overlays.flatMap((overlay, index) => (state.selected.has(overlay) ? [index] : []))
        : []
    };
  }

  function pushHistory(stack, entry) {
    stack.push(entry);
    if (stack.length > HISTORY_LIMIT) stack.shift();
  }

  // Records `before` as one undo step when the edit changed profiles or the
  // active profile. Repeated edits sharing `coalesceKey` within a short window
  // (wheel layering, profile position stepping) fold into the first step.
  function commitHistory(before, coalesceKey = "") {
    if (!before || !state.config) return false;
    if (before.active_profile === state.config.active_profile
        && before.profiles === JSON.stringify(state.config.profiles)) return false;
    const now = performance.now();
    const coalesce = Boolean(coalesceKey) && coalesceKey === history.lastKey
      && now - history.lastAt < HISTORY_COALESCE_MS && history.undo.length > 0;
    if (!coalesce) pushHistory(history.undo, before);
    history.redo.length = 0;
    history.lastKey = coalesceKey;
    history.lastAt = now;
    requestRender("chrome");
    return true;
  }

  function resetHistory() {
    history.undo.length = 0;
    history.redo.length = 0;
    history.lastKey = "";
    state.inspectorEdit = null;
    requestRender("chrome");
  }

  // Restored snapshots are fresh objects; reusing the live profile and overlay
  // objects keeps their iframes mounted so undo/redo does not reload every
  // overlay. Matching is by source, in order, within the same profile.
  function reconcileProfiles(nextProfiles) {
    const currentById = new Map(state.config.profiles.map((profile) => [profile.id, profile]));
    return nextProfiles.map((next) => {
      const current = currentById.get(next.id);
      if (!current) return next;
      const pool = current.overlays.slice();
      current.name = next.name;
      current.overlays = next.overlays.map((overlay) => {
        const index = pool.findIndex((candidate) => candidate
          && candidate.counter_id === overlay.counter_id && candidate.url === overlay.url);
        if (index < 0) return overlay;
        const existing = pool[index];
        pool[index] = null;
        for (const key of Object.keys(existing)) if (!(key in overlay)) delete existing[key];
        return Object.assign(existing, overlay);
      });
      return current;
    });
  }

  function stepHistory(from, to, verb) {
    if (!state.config) return;
    closeProfileMenu();
    closeProfileCreator();
    closeProfileDeleteConfirm();
    closeInspectorPopup();
    // Ends drags and blurs inspector fields first so their pending edit becomes
    // its own step (and clears redo) before this one is applied.
    flushEditingChanges();
    const entry = from.pop();
    if (!entry) {
      status.textContent = `Nothing to ${verb}`;
      requestRender("chrome");
      return;
    }
    const current = historySnapshot();
    // Both directions stay in the profile the edit happened in, so redo shows
    // the change it reapplies even after the user switched profiles.
    if (current.active_profile !== entry.active_profile
        && state.config.profiles.some((profile) => profile.id === entry.active_profile)) {
      current.active_profile = entry.active_profile;
      current.selected = [];
    }
    pushHistory(to, current);
    history.lastKey = "";
    replaceSelection([]);
    const profiles = reconcileProfiles(JSON.parse(entry.profiles));
    state.config.profiles = profiles;
    state.config.active_profile = profiles.some((profile) => profile.id === entry.active_profile)
      ? entry.active_profile : profiles[0].id;
    const profile = activeProfile();
    replaceSelection(entry.selected.map((index) => profile.overlays[index]).filter(Boolean));
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  }

  function undoEdit() {
    stepHistory(history.undo, history.redo, "undo");
  }

  function redoEdit() {
    stepHistory(history.redo, history.undo, "redo");
  }

  function beginInspectorEdit() {
    if (!state.inspectorEdit) state.inspectorEdit = { before: historySnapshot(), committed: false };
  }

  // Inspector fields mutate live while typing; the whole focus session is one
  // step, committed on the first real change so Undo is available immediately.
  function commitInspectorEdit() {
    const edit = state.inspectorEdit;
    if (edit && !edit.committed) edit.committed = commitHistory(edit.before);
  }

  function endInspectorEdit() {
    commitInspectorEdit();
    state.inspectorEdit = null;
  }

  function setError(message) {
    state.error = message || "Unknown overlay error";
    status.textContent = state.error;
    toast(state.error, true);
  }

  function toast(message, isError = false) {
    const item = document.createElement("div");
    item.className = isError ? "toast error" : "toast";
    item.textContent = message;
    $("toast-region").append(item);
    window.setTimeout(() => item.remove(), 3600);
  }

  function makeElement(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }

  function overlayDetails(overlay) {
    const counter = findCounter(overlay.counter_id);
    if (overlay.url) {
      const source = customSource(overlay);
      return {
        available: Boolean(source),
        name: safeId(overlay.name, source ? source.hostname || "HTML overlay" : overlay.counter_id),
        url: source ? source.href : null,
        counter: null
      };
    }
    return {
      available: Boolean(counter),
      name: safeId(overlay.name, counter ? counter.name : `${overlay.counter_id} (missing)`),
      url: counter ? counter.url : null,
      counter
    };
  }

  function overlayElement(overlay, details = overlayDetails(overlay)) {
    const enabled = overlay.enabled !== false;
    const root = makeElement("div", `overlay${details.available && enabled ? "" : " missing"}${enabled ? "" : " disabled"}${state.selected.has(overlay) ? " selected" : ""}`);
    root.dataset.counterId = overlay.counter_id;
    root._vosulayOverlay = overlay;
    root._vosulaySourceIdentity = `${enabled}:${details.available ? "available" : "missing"}\n${details.url || ""}\n${overlay.url ? "custom" : "counter"}`;
    applyOverlayStyle(root, overlay);

    if (enabled && details.available) {
      const frame = makeElement("iframe", "overlay-frame");
      frame.src = typeof window.__VOSULAY_BACKEND_URL__ === "function"
        ? window.__VOSULAY_BACKEND_URL__(details.url) : details.url;
      frame.title = details.name;
      frame.setAttribute("allow", "autoplay");
      if (overlay.url) {
        frame.setAttribute("sandbox", "allow-scripts allow-same-origin");
      }
      frame.setAttribute("loading", "eager");
      root.append(frame);
    } else {
      const missing = makeElement("div", "missing-card");
      missing.append(makeElement("strong", "", enabled ? "Counter unavailable" : "Disabled"));
      missing.append(makeElement("small", "", enabled ? overlay.counter_id : `${details.name} · disabled`));
      root.append(missing);
    }

    root.append(makeElement("div", "interaction-shield"));
    root.append(makeElement("div", "overlay-tag", details.name));
    for (const edge of ["nw", "ne", "sw", "se"]) {
      const handle = makeElement("div", "resize-handle");
      handle.dataset.edge = edge;
      root.append(handle);
    }
    return root;
  }

  function renderStage(structureDirty, geometryDirty) {
    const profile = activeProfile();
    const overlays = profile ? profile.overlays : [];
    const expectedOverlays = new Set(overlays);

    if (structureDirty) {
      for (const [overlay, root] of overlayRoots) {
        if (!expectedOverlays.has(overlay)) {
          root.remove();
          overlayRoots.delete(overlay);
        }
      }

      for (const overlay of overlays) {
        const details = overlayDetails(overlay);
        const identity = `${overlay.enabled !== false}:${details.available ? "available" : "missing"}\n${details.url || ""}\n${overlay.url ? "custom" : "counter"}`;
        let root = overlayRoots.get(overlay);
        if (root && root._vosulaySourceIdentity !== identity) {
          const replacement = overlayElement(overlay, details);
          root.replaceWith(replacement);
          root = replacement;
          overlayRoots.set(overlay, root);
        } else if (!root) {
          root = overlayElement(overlay, details);
          overlayRoots.set(overlay, root);
          stage.append(root);
        }
        const tag = root.querySelector(".overlay-tag");
        if (tag && tag.textContent !== details.name) tag.textContent = details.name;
        const frame = root.querySelector(".overlay-frame");
        if (frame && frame.title !== details.name) frame.title = details.name;
      }

      let cursor = stage.firstElementChild;
      for (const overlay of overlays) {
        const root = overlayRoots.get(overlay);
        if (root !== cursor) stage.insertBefore(root, cursor);
        cursor = root.nextElementSibling;
      }
    }

    if (structureDirty || geometryDirty) {
      for (const overlay of overlays) {
        const soleSelected = state.selected.size === 1 && state.selected.has(overlay);
        if (soleSelected) {
          const previous = `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}`;
          clampOverlay(overlay);
          // Persist the pull-into-view so the saved placement matches the screen.
          if (previous !== `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}`) scheduleSave();
        }
        const root = overlayRoots.get(overlay);
        if (!root) continue;
        applyOverlayStyle(root, overlay);
        root.classList.toggle("selected", state.selected.has(overlay));
        for (const handle of root.querySelectorAll(".resize-handle")) handle.hidden = !soleSelected;
      }
    }
  }

  function closeProfileMenu(refocus = false) {
    profileMenu.hidden = true;
    profileSelect.setAttribute("aria-expanded", "false");
    if (refocus) profileSelect.focus();
  }

  function openProfileMenu(focusTarget = "") {
    const options = [...profileMenu.querySelectorAll(".profile-option")];
    if (!options.length) return;
    profileMenu.hidden = false;
    profileSelect.setAttribute("aria-expanded", "true");
    if (!focusTarget) return;
    const active = options.find((option) => option.getAttribute("aria-checked") === "true");
    const target = focusTarget === "first"
      ? options[0]
      : focusTarget === "last"
        ? options[options.length - 1]
        : active || options[0];
    options.forEach((option) => { option.tabIndex = option === target ? 0 : -1; });
    target.focus();
  }

  function toggleProfileMenu(focusTarget = "") {
    if (profileMenu.hidden) openProfileMenu(focusTarget);
    else closeProfileMenu();
  }

  function activateProfile(id) {
    if (!state.config || !state.config.profiles.some((profile) => profile.id === id)) return;
    flushEditingChanges();
    replaceSelection([]);
    state.config.active_profile = id;
    scheduleSave(true);
    closeProfileMenu(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  }

  function renderProfiles() {
    closeProfileMenu();
    profileMenu.replaceChildren();
    if (!state.config) {
      profileSelectLabel.textContent = "";
      profileImport.disabled = true;
      profileExport.disabled = true;
      profilePosition.disabled = true;
      profilePosition.value = "";
      profilePositionTotal.textContent = "";
      return;
    }
    const current = activeProfile();
    const count = state.config.profiles.length;
    profileSelectLabel.textContent = current ? current.name : "";
    state.config.profiles.forEach((profile, index) => {
      const option = makeElement("button", "profile-option");
      const active = profile === current;
      option.type = "button";
      option.dataset.profileId = profile.id;
      option.setAttribute("role", "menuitemradio");
      option.setAttribute("aria-checked", String(active));
      option.tabIndex = active ? 0 : -1;
      option.append(
        makeElement("span", "profile-option-index", String(index + 1)),
        makeElement("span", "profile-option-name", profile.name)
      );
      profileMenu.append(option);
    });
    profilePosition.max = String(count);
    profilePosition.disabled = !current || count <= 1;
    profilePosition.removeAttribute("aria-invalid");
    if (document.activeElement !== profilePosition) {
      profilePosition.value = current ? String(state.config.profiles.indexOf(current) + 1) : "";
    }
    profilePositionTotal.textContent = `of ${count}`;
    profileDelete.disabled = !current;
    profileImport.disabled = !current;
    profileExport.disabled = !current;
  }

  function renderCatalog() {
    catalog.replaceChildren();
    const counters = state.counters;
    if (counters === null) {
      catalog.append(makeElement("div", "catalog-empty", "Waiting for installed counters…"));
      $("catalog-summary").textContent = "Connecting…";
      return;
    }
    $("catalog-summary").textContent = counters.length === 1 ? "1 counter" : `${counters.length} counters`;
    if (!counters.length) {
      catalog.append(makeElement("div", "catalog-empty", "No installed counters found. Install counters in vosu, then refresh this list."));
      return;
    }
    const profile = activeProfile();
    const used = new Set(profile ? profile.overlays.map((overlay) => overlay.counter_id) : []);
    for (const counter of counters) {
      const exists = used.has(counter.id);
      const button = makeElement("button", `catalog-item${exists ? " used" : ""}`);
      button.type = "button";
      button.dataset.counterId = counter.id;
      button.title = exists ? "Add another instance of this counter" : "Add this counter";
      const copy = makeElement("span", "catalog-copy");
      copy.append(makeElement("strong", "", counter.name));
      const detail = [counter.author, counter.version].filter(Boolean).join(" · ") || counter.id;
      copy.append(makeElement("small", "", detail));
      button.append(copy, makeElement("span", "catalog-add", exists ? "✓" : "+"));
      catalog.append(button);
    }
  }

  function inspectorFieldValue(name, overlay) {
    if (name === "z_index") return String(overlay.z_index);
    if (name === "opacity") return String(Math.round(overlay.opacity * 10000) / 100);
    return String(Math.round(overlay[name] * 100) / 100);
  }

  function fieldWarning(input) {
    return document.getElementById(`${input.id}-warning`);
  }

  function clearFieldWarning(input) {
    const warning = fieldWarning(input);
    input.removeAttribute("aria-invalid");
    if (!warning) return;
    warning.textContent = "";
    warning.hidden = true;
  }

  function showFieldWarning(input, message) {
    const warning = fieldWarning(input);
    input.setAttribute("aria-invalid", "true");
    if (!warning) return;
    warning.textContent = message;
    warning.hidden = false;
  }

  function clearInspectorWarnings() {
    for (const input of Object.values(fields)) clearFieldWarning(input);
  }

  function inspectorFieldBounds(input) {
    return {
      minimum: input.min === "" ? -Infinity : finite(input.min, -Infinity),
      maximum: input.max === "" ? Infinity : finite(input.max, Infinity)
    };
  }

  function applyInspectorFieldValue(name, overlay, value) {
    if (name === "z_index") overlay.z_index = Math.round(value);
    else if (name === "opacity") overlay.opacity = value / 100;
    else overlay[name] = value;
    clampOverlay(overlay);
  }

  let inspectorWarningContext = null;
  const positionFocusValues = new WeakMap();

  function renderInspectorNumbers(overlay, names = Object.keys(fields)) {
    for (const name of names) {
      const input = fields[name];
      if (document.activeElement === input) continue;
      const value = inspectorFieldValue(name, overlay);
      if (input.value !== value) input.value = value;
    }
  }

  function renderInspectorText(input, value) {
    if (document.activeElement !== input && input.value !== value) input.value = value;
  }

  function renderInspector() {
    const overlay = selectedOverlay();
    document.body.classList.toggle("context-inspector", state.inspectorPopup);
    const warningContext = state.editing ? overlay : null;
    if (inspectorWarningContext !== warningContext) {
      inspectorWarningContext = warningContext;
      clearInspectorWarnings();
    }
    inspector.hidden = !state.editing || !overlay || !state.inspectorPopup;
    if (inspector.hidden) return;
    if (state.inspectorPoint) {
      inspector.style.right = "auto";
      inspector.style.left = `${state.inspectorPoint.x}px`;
      inspector.style.top = `${state.inspectorPoint.y}px`;
      const rect = inspector.getBoundingClientRect();
      inspector.style.left = `${clamp(state.inspectorPoint.x, 0, Math.max(0, window.innerWidth - rect.width))}px`;
      inspector.style.top = `${clamp(state.inspectorPoint.y, 0, Math.max(0, window.innerHeight - rect.height))}px`;
    } else {
      inspector.style.removeProperty("left");
      inspector.style.removeProperty("top");
      inspector.style.removeProperty("right");
    }
    const counter = findCounter(overlay.counter_id);
    const details = overlayDetails(overlay);
    const custom = Boolean(overlay.url);

    $("selection-name").textContent = details.name;
    if (custom) {
      const source = customSource(overlay);
      $("selection-meta").textContent = source
        ? `${source.protocol === "vosulay-overlay:" ? "HTML file" : "URL"} · ${overlay.url}`
        : `Unsupported source · ${overlay.url}`;
    } else {
      $("selection-meta").textContent = counter ? [counter.author, counter.version].filter(Boolean).join(" · ") : "Counter is not installed";
    }

    renderInspectorText(nameField, details.name);
    urlFieldLabel.hidden = !custom;
    renderInspectorText(urlField, custom ? overlay.url : "");
    enabledField.checked = overlay.enabled !== false;
    renderInspectorNumbers(overlay);
    $("settings").disabled = custom || !counter || !counter.has_settings;
  }

  function describeShortcut(button, label, shortcut) {
    const title = shortcut ? `${label} (${shortcut})` : label;
    if (button.title !== title) button.title = title;
    const keys = shortcut
      ? shortcut.split("+").map((part) => (part === "Ctrl" ? "Control" : part === "Plus" ? "+" : part === "Minus" ? "-" : part)).join("+")
      : "";
    if (keys) button.setAttribute("aria-keyshortcuts", keys);
    else button.removeAttribute("aria-keyshortcuts");
  }

  function renderToolbarActions() {
    const hotkeys = state.config ? state.config.settings.hotkeys : DEFAULT_HOTKEYS;
    const profile = activeProfile();
    undoButton.disabled = !history.undo.length;
    redoButton.disabled = !history.redo.length;
    refreshSelectedButton.disabled = !profile || !profile.overlays.some((overlay) => state.selected.has(overlay));
    refreshAllButton.disabled = !profile || !profile.overlays.length;
    describeShortcut(undoButton, "Undo", hotkeys.undo);
    describeShortcut(redoButton, "Redo", hotkeys.redo);
    describeShortcut(refreshSelectedButton, "Refresh selected overlays", hotkeys.refresh_selected_overlay);
    describeShortcut(refreshAllButton, "Refresh all overlays", hotkeys.refresh_all_overlays);
  }

  function render() {
    const regions = { ...dirty };
    for (const name of Object.keys(dirty)) dirty[name] = false;
    if (regions.chrome) {
      document.body.classList.toggle("editing", state.editing);
      document.body.classList.toggle("interacting", state.editing && state.interacting);
      editor.hidden = !state.editing;
    }
    if (regions.stageStructure || regions.stageGeometry) {
      renderStage(regions.stageStructure, regions.stageGeometry);
    }
    if (regions.profileList) renderProfiles();
    if (regions.catalog) renderCatalog();
    if (regions.inspector) renderInspector();
    renderToolbarActions();
    updatePanelAvoidance();
  }

  function closeInspectorPopup(preserveCycle = false) {
    if (!state.inspectorPopup && !state.inspectorPoint) return;
    state.inspectorPopup = false;
    state.inspectorPoint = null;
    if (!preserveCycle) state.inspectorCycle = null;
    requestRender("inspector");
  }

  function replaceSelection(overlays, { keepPopup = false } = {}) {
    const profile = activeProfile();
    const next = new Set(profile ? [...overlays].filter((overlay) => profile.overlays.includes(overlay)) : []);
    const same = next.size === state.selected.size && [...next].every((overlay) => state.selected.has(overlay));
    if (!same && state.selected.size === 1 && inspector.contains(document.activeElement)) {
      document.activeElement.blur();
    }
    if (!keepPopup) {
      state.inspectorCycle = null;
      state.inspectorPopup = false;
      state.inspectorPoint = null;
    }
    if (same) {
      if (!keepPopup) requestRender("inspector");
      return;
    }
    state.selected.clear();
    for (const overlay of next) state.selected.add(overlay);
    requestRender("stageGeometry", "inspector");
  }

  function selectOverlay(overlay, options) {
    replaceSelection(overlay ? [overlay] : [], options);
  }

  function uniqueProfileId() {
    const taken = new Set(state.config.profiles.map((profile) => profile.id));
    let id;
    do id = `profile-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    while (taken.has(id));
    return id;
  }
  function prepareProfileImport(input) {
    if (!state.config) throw new Error("Cannot import a profile before configuration is loaded");
    if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error("Imported profile must be an object");
    const name = typeof input.name === "string" ? input.name.trim() : "";
    if (!name) throw new Error("Imported profile name cannot be empty");
    if (!Array.isArray(input.overlays)) throw new Error("Imported profile overlays must be an array");
    if (input.overlays.some((overlay) => !overlay || typeof overlay !== "object" || Array.isArray(overlay))) {
      throw new Error("Every imported overlay must be an object");
    }

    const overlays = input.overlays.map(normalizeOverlay);
    const invalidSource = overlays.find((overlay) => overlay.url && !customSource(overlay));
    if (invalidSource) throw new Error(`Unsupported custom overlay source: ${invalidSource.url}`);

    ipc({ type: "authorize-profile-import", profile: { id: uniqueProfileId(), name, overlays } });
  }

  function commitProfileImport(profile) {
    if (state.config.profiles.some((existing) => existing.id === profile.id)) profile = { ...profile, id: uniqueProfileId() };
    flushEditingChanges();
    const before = historySnapshot();
    replaceSelection([]);
    state.config.profiles.push(profile);
    state.config.active_profile = profile.id;
    commitHistory(before);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
    status.textContent = "Profile imported";
    toast(`Imported “${profile.name}”`);
  }

  function addCounter(counter) {
    const profile = activeProfile();
    if (!profile) return;
    const before = historySnapshot();
    const width = Math.max(10, finite(counter.width, 500));
    const height = Math.max(10, finite(counter.height, 300));
    const highest = profile.overlays.reduce((value, overlay) => Math.max(value, overlay.z_index), -1);
    const overlay = normalizeOverlay({
      counter_id: counter.id,
      width, height, scale: 1,
      left: (window.innerWidth - width) / 2,
      top: (window.innerHeight - height) / 2,
      z_index: clamp(highest + 1, 0, 999)
    });
    profile.overlays.push(overlay);
    selectOverlay(overlay);
    clampOverlay(overlay);
    commitHistory(before);
    scheduleSave();
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function uniqueCustomId() {
    const taken = new Set(state.config.profiles.flatMap((profile) => profile.overlays.map((overlay) => overlay.counter_id)));
    let id;
    do id = `custom:${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
    while (taken.has(id));
    return id;
  }

  function deriveUrlName(url) {
    const part = url.pathname.split("/").filter(Boolean).pop();
    if (part) {
      try {
        return decodeURIComponent(part).replace(/\.html?$/i, "") || url.hostname;
      } catch (_) {
        return part.replace(/\.html?$/i, "") || url.hostname;
      }
    }
    return url.hostname;
  }

  function addCustomOverlay(url, name) {
    const profile = activeProfile();
    if (!profile) return;
    const before = historySnapshot();
    const width = 500;
    const height = 300;
    const highest = profile.overlays.reduce((value, overlay) => Math.max(value, overlay.z_index), -1);
    const overlay = normalizeOverlay({
      counter_id: uniqueCustomId(),
      url: url.href,
      name: safeId(name, deriveUrlName(url) || "Custom overlay"),
      width,
      height,
      scale: 1,
      left: (window.innerWidth - width) / 2,
      top: (window.innerHeight - height) / 2,
      z_index: clamp(highest + 1, 0, 999)
    });
    profile.overlays.push(overlay);
    selectOverlay(overlay);
    clampOverlay(overlay);
    commitHistory(before);
    scheduleSave();
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function removeSelected() {
    const profile = activeProfile();
    const overlay = selectedOverlay();
    if (!profile || !overlay) return;
    const index = profile.overlays.indexOf(overlay);
    if (index < 0) return;
    const before = historySnapshot();
    replaceSelection([]);
    profile.overlays.splice(index, 1);
    commitHistory(before);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function refreshOverlayFrames(overlays) {
    let refreshed = 0;
    for (const overlay of overlays) {
      const frame = overlayRoots.get(overlay)?.querySelector(".overlay-frame");
      if (!frame) continue;
      // Reassigning the source reloads cross-origin frames, which the page
      // cannot reach through contentWindow.
      frame.src = frame.src;
      refreshed += 1;
    }
    return refreshed;
  }

  function refreshSelectedOverlays() {
    const profile = activeProfile();
    const selected = profile ? profile.overlays.filter((overlay) => state.selected.has(overlay)) : [];
    if (!selected.length) {
      status.textContent = "Select an overlay to refresh";
      return;
    }
    const refreshed = refreshOverlayFrames(selected);
    status.textContent = refreshed
      ? `Refreshed ${refreshed} selected overlay${refreshed === 1 ? "" : "s"}`
      : "Selected overlays are disabled or unavailable";
  }

  function refreshAllOverlays() {
    const profile = activeProfile();
    const refreshed = profile ? refreshOverlayFrames(profile.overlays) : 0;
    status.textContent = refreshed
      ? `Refreshed ${refreshed} overlay${refreshed === 1 ? "" : "s"}`
      : "No loaded overlays to refresh";
  }

  function flushEditingChanges() {
    if (inspector.contains(document.activeElement)) document.activeElement.blur();
    const operation = state.pointer;
    if (operation) {
      if (operation.frame) {
        cancelAnimationFrame(operation.frame);
        operation.frame = 0;
        flushPointer(operation);
      }
      state.pointer = null;
      if (operation.root.hasPointerCapture(operation.id)) operation.root.releasePointerCapture(operation.id);
      if (operation.kind === "selection") selectionBox.hidden = true;
      else commitHistory(operation.before);
    }
    flushPendingSave();
  }

  function closeUrlModal(restoreFocus = true) {
    if (urlModal.hidden) return;
    urlModal.hidden = true;
    if (restoreFocus) addUrl.focus();
  }

  function openUrlModal() {
    if (!state.config) return;
    closeProfileMenu();
    closeProfileCreator();
    closeProfileDeleteConfirm();
    urlName.value = "";
    urlInput.value = "https://";
    urlModal.hidden = false;
    urlName.focus();
  }

  function closeProfileCreator(restoreFocus = false) {
    if (profileCreate.hidden) return;
    profileCreate.hidden = true;
    profileActions.hidden = false;
    if (restoreFocus) profileAdd.focus();
  }

  function openProfileCreator() {
    if (!state.config) return;
    closeProfileDeleteConfirm();
    profileName.value = `Profile ${state.config.profiles.length + 1}`;
    profileActions.hidden = true;
    profileCreate.hidden = false;
    profileName.focus();
    profileName.select();
  }

  function closeProfileDeleteConfirm(restoreFocus = false) {
    if (profileDeleteConfirm.hidden) return;
    profileDeleteConfirm.hidden = true;
    delete profileDeleteConfirm.dataset.profileId;
    profileActions.hidden = false;
    if (restoreFocus) profileDelete.focus();
    updateToolbarAvoidance();
  }

  // window.confirm has no UI in the off-screen browser and returns false, so
  // deletion is confirmed inline in the toolbar instead.
  function openProfileDeleteConfirm() {
    const profile = activeProfile();
    if (!state.config || !profile) return;
    closeProfileMenu();
    closeProfileCreator();
    profileDeleteMessage.textContent = state.config.profiles.length > 1
      ? `Delete “${profile.name}”?`
      : `Delete “${profile.name}”? An empty Default profile replaces it.`;
    profileDeleteMessage.title = profileDeleteMessage.textContent;
    profileDeleteConfirm.dataset.profileId = profile.id;
    profileActions.hidden = true;
    profileDeleteConfirm.hidden = false;
    profileDeleteCancel.focus();
  }

  function deleteConfirmedProfile() {
    if (!state.config) return;
    const profiles = state.config.profiles;
    const profile = profiles.find((item) => item.id === profileDeleteConfirm.dataset.profileId);
    if (!profile) {
      closeProfileDeleteConfirm(true);
      return;
    }
    flushEditingChanges();
    const before = historySnapshot();
    const wasActive = profile === activeProfile();
    replaceSelection([]);
    const index = profiles.indexOf(profile);
    profiles.splice(index, 1);
    // A configuration always needs one profile; the sole profile is replaced
    // by an empty one rather than refusing the deletion.
    if (!profiles.length) profiles.push({ id: uniqueProfileId(), name: "Default", overlays: [] });
    if (wasActive || !profiles.some((item) => item.id === state.config.active_profile)) {
      state.config.active_profile = profiles[Math.min(index, profiles.length - 1)].id;
    }
    commitHistory(before);
    closeProfileDeleteConfirm(true);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
    const undo = state.config.settings.hotkeys.undo;
    toast(`Deleted “${profile.name}”${undo ? `. Press ${undo} to undo.` : ""}`);
  }

  function commitProfilePosition() {
    const profile = activeProfile();
    if (!state.config || !profile) return;
    const profiles = state.config.profiles;
    const current = profiles.indexOf(profile) + 1;
    const raw = profilePosition.value.trim();
    const requested = Number(raw);
    if (!raw || !Number.isInteger(requested) || requested < 1 || requested > profiles.length) {
      profilePosition.value = String(current);
      profilePosition.removeAttribute("aria-invalid");
      setError(`Profile position must be a whole number from 1 to ${profiles.length}`);
      return;
    }
    profilePosition.removeAttribute("aria-invalid");
    if (requested === current) return;
    const before = historySnapshot();
    profiles.splice(current - 1, 1);
    profiles.splice(requested - 1, 0, profile);
    commitHistory(before, `profile-position:${profile.id}`);
    scheduleSave(true);
    requestRender("profileList");
  }

  function setEditing(editing, notify = true) {
    const next = Boolean(editing);
    const changed = state.editing !== next;
    const interactionChanged = !next && state.interacting;
    if (!next) {
      state.interacting = false;
      closeUrlModal();
      closeSettingsModal(false);
      closeProfileMenu();
      closeProfileCreator();
      closeProfileDeleteConfirm();
      closeInspectorPopup();
    }
    if (!next) flushEditingChanges();
    if (changed) {
      state.editing = next;
      if (!next) replaceSelection([]);
    }
    if (changed || interactionChanged) requestRender("chrome", "stageGeometry", "inspector");
    if (notify && changed) ipc({ type: "set-editing", editing: next });
    else if (!notify && !next) ipc({ type: "editing-applied", editing: false });
  }

  function startPointer(event, root, edge) {
    if (!state.editing || event.button !== 0) return;
    const overlay = root._vosulayOverlay;
    if (!overlay || !state.selected.has(overlay)) return;
    if (edge && selectedOverlay() !== overlay) return;
    event.preventDefault();
    if (inspector.contains(document.activeElement) || toolbar.contains(document.activeElement)) {
      document.activeElement.blur();
    }
    root.setPointerCapture(event.pointerId);
    const before = historySnapshot();
    const angle = overlay.rotation * Math.PI / 180;
    const placements = [...state.selected].map((placement) => ({
      overlay: placement,
      left: placement.left,
      top: placement.top
    }));
    // Placements already outside their range (never clamped while unselected)
    // may not be pushed further out, but the drag must not snap them back.
    const movementBounds = placements.reduce((bounds, placement) => {
      const range = overlayPositionRange(placement.overlay);
      bounds.minX = Math.max(bounds.minX, Math.min(0, range.minLeft - placement.left));
      bounds.maxX = Math.min(bounds.maxX, Math.max(0, range.maxLeft - placement.left));
      bounds.minY = Math.max(bounds.minY, Math.min(0, range.minTop - placement.top));
      bounds.maxY = Math.min(bounds.maxY, Math.max(0, range.maxTop - placement.top));
      return bounds;
    }, { minX: -Infinity, maxX: Infinity, minY: -Infinity, maxY: Infinity });
    state.pointer = {
      kind: "overlay",
      before,
      id: event.pointerId,
      root,
      overlay,
      placements,
      movementBounds,
      edge,
      x: event.clientX,
      y: event.clientY,
      latestX: event.clientX,
      latestY: event.clientY,
      left: overlay.left,
      top: overlay.top,
      width: overlay.width,
      height: overlay.height,
      scale: overlay.scale,
      cosine: Math.cos(angle),
      sine: Math.sin(angle),
      inverseScale: 1 / overlay.scale,
      frame: 0
    };
  }

  function flushPointer(operation) {
    if (!operation || state.pointer !== operation) return;
    operation.frame = 0;
    const screenX = operation.latestX - operation.x;
    const screenY = operation.latestY - operation.y;
    if (operation.kind === "selection") {
      const left = Math.min(operation.x, operation.latestX);
      const top = Math.min(operation.y, operation.latestY);
      const right = Math.max(operation.x, operation.latestX);
      const bottom = Math.max(operation.y, operation.latestY);
      selectionBox.style.left = `${left}px`;
      selectionBox.style.top = `${top}px`;
      selectionBox.style.width = `${right - left}px`;
      selectionBox.style.height = `${bottom - top}px`;
      const profile = activeProfile();
      replaceSelection((profile ? profile.overlays : []).filter((overlay) => {
        const rect = overlayRoots.get(overlay)?.getBoundingClientRect();
        return rect && rect.right >= left && rect.left <= right && rect.bottom >= top && rect.top <= bottom;
      }));
      return;
    }

    const overlay = operation.overlay;
    let affected;
    if (!operation.edge) {
      const movementX = clamp(screenX, operation.movementBounds.minX, operation.movementBounds.maxX);
      const movementY = clamp(screenY, operation.movementBounds.minY, operation.movementBounds.maxY);
      for (const placement of operation.placements) {
        placement.overlay.left = placement.left + movementX;
        placement.overlay.top = placement.top + movementY;
        const root = overlayRoots.get(placement.overlay);
        if (root) applyOverlayStyle(root, placement.overlay);
      }
      affected = ["left", "top"];
    } else {
      const dx = (operation.cosine * screenX + operation.sine * screenY) * operation.inverseScale;
      const dy = (-operation.sine * screenX + operation.cosine * screenY) * operation.inverseScale;
      overlay.left = operation.left;
      overlay.top = operation.top;
      overlay.width = operation.width;
      overlay.height = operation.height;

      if (operation.edge.includes("e")) overlay.width = Math.max(10, operation.width + dx);
      if (operation.edge.includes("s")) overlay.height = Math.max(10, operation.height + dy);
      if (operation.edge.includes("w")) {
        overlay.width = Math.max(10, operation.width - dx);
        const shift = (operation.width - overlay.width) * operation.scale;
        overlay.left += operation.cosine * shift;
        overlay.top += operation.sine * shift;
      }
      if (operation.edge.includes("n")) {
        overlay.height = Math.max(10, operation.height - dy);
        const shift = (operation.height - overlay.height) * operation.scale;
        overlay.left -= operation.sine * shift;
        overlay.top += operation.cosine * shift;
      }
      affected = ["left", "top", "width", "height"];
      clampOverlay(overlay);
      applyOverlayStyle(operation.root, overlay);
    }
    updatePanelAvoidance();
    if (state.editing && selectedOverlay() === overlay) renderInspectorNumbers(overlay, affected);
    scheduleSave();
  }

  function movePointer(event) {
    const operation = state.pointer;
    if (!operation || operation.id !== event.pointerId) return;
    operation.latestX = event.clientX;
    operation.latestY = event.clientY;
    if (!operation.frame) operation.frame = requestAnimationFrame(() => flushPointer(operation));
  }

  function endPointer(event) {
    const operation = state.pointer;
    if (!operation || operation.id !== event.pointerId) return;
    operation.latestX = event.clientX;
    operation.latestY = event.clientY;
    if (operation.frame) {
      cancelAnimationFrame(operation.frame);
      operation.frame = 0;
    }
    flushPointer(operation);
    state.pointer = null;
    if (operation.root.hasPointerCapture(operation.id)) operation.root.releasePointerCapture(operation.id);
    if (operation.kind === "selection") {
      selectionBox.hidden = true;
    } else {
      commitHistory(operation.before);
      scheduleSave(true);
    }
    requestRender("stageGeometry", "inspector");
    updateToolbarAvoidance();
    releaseToolbarHold(event.clientX, event.clientY);
  }

  stage.addEventListener("pointerdown", (event) => {
    if (!state.editing || event.button !== 0) return;
    const root = event.target.closest(".overlay");
    if (!root) {
      event.preventDefault();
      replaceSelection([]);
      stage.setPointerCapture(event.pointerId);
      selectionBox.hidden = false;
      state.pointer = {
        kind: "selection",
        id: event.pointerId,
        root: stage,
        x: event.clientX,
        y: event.clientY,
        latestX: event.clientX,
        latestY: event.clientY,
        frame: 0
      };
      flushPointer(state.pointer);
      return;
    }
    const overlay = root._vosulayOverlay;
    if (!state.selected.has(overlay)) selectOverlay(overlay);
    const edge = event.target.closest(".resize-handle")?.dataset.edge || null;
    startPointer(event, root, edge);
  });
  stage.addEventListener("pointermove", movePointer);
  stage.addEventListener("pointerup", endPointer);
  stage.addEventListener("pointercancel", endPointer);
  document.addEventListener("pointermove", (event) => {
    if (state.toolbarHold) releaseToolbarHold(event.clientX, event.clientY);
  }, { passive: true });
  document.documentElement.addEventListener("pointerleave", () => releaseToolbarHold(NaN, NaN));
  toolbar.addEventListener("focusin", updateToolbarAvoidance);
  function showContextInspector(event, root = null) {
    if (!state.editing) return;
    const point = root
      ? { x: event.clientX, y: event.clientY }
      : state.inspectorPoint;
    const clickedOverlay = root ? root._vosulayOverlay : null;
    const overlays = point ? overlaysAtPoint(point.x, point.y) : [];
    if (root && !overlays.length && clickedOverlay) overlays.push(clickedOverlay);
    if (!root && overlays.length < 2) return;
    const overlay = contextOverlay(overlays, clickedOverlay);
    if (!overlay) return;
    event.preventDefault();
    selectOverlay(overlay, { keepPopup: true });
    state.inspectorPopup = true;
    if (root) state.inspectorPoint = point;
    requestRender("inspector");
  }

  stage.addEventListener("contextmenu", (event) => {
    const root = event.target.closest(".overlay");
    if (!root) return;
    showContextInspector(event, root);
  });

  editor.addEventListener("contextmenu", (event) => {
    if (inspector.contains(event.target)) showContextInspector(event);
  });
  stage.addEventListener("wheel", (event) => {
    if (event.ctrlKey || !state.editing) return;
    const root = event.target.closest(".overlay");
    if (!root) return;
    event.preventDefault();
    selectOverlay(root._vosulayOverlay);
    const overlay = selectedOverlay();
    if (!overlay) return;
    const before = historySnapshot();
    overlay.z_index = clamp(overlay.z_index + (event.deltaY < 0 ? 1 : -1), 0, 999);
    applyOverlayStyle(root, overlay);
    renderInspectorNumbers(overlay, ["z_index"]);
    commitHistory(before, `layer:${activeProfile().overlays.indexOf(overlay)}`);
    scheduleSave();
  }, { passive: false });

  catalog.addEventListener("click", (event) => {
    const button = event.target.closest(".catalog-item");
    if (!button || !state.counters) return;
    const counter = findCounter(button.dataset.counterId);
    if (counter) addCounter(counter);
  });

  profileSelect.addEventListener("click", () => toggleProfileMenu());
  profileSelect.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (event.repeat) return;
      toggleProfileMenu("active");
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openProfileMenu(event.key === "ArrowUp" ? "last" : "first");
    } else if (event.key === "Escape" && !profileMenu.hidden) {
      event.preventDefault();
      event.stopPropagation();
      closeProfileMenu();
    }
  });
  profileMenu.addEventListener("click", (event) => {
    const option = event.target.closest(".profile-option");
    if (option) activateProfile(option.dataset.profileId);
  });
  profileMenu.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeProfileMenu(true);
      return;
    }
    if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    const options = [...profileMenu.querySelectorAll(".profile-option")];
    if (!options.length) return;
    event.preventDefault();
    const current = options.indexOf(document.activeElement);
    let next = 0;
    if (event.key === "End") next = options.length - 1;
    else if (event.key === "ArrowUp") next = current <= 0 ? options.length - 1 : current - 1;
    else if (event.key === "ArrowDown") next = current < 0 || current === options.length - 1 ? 0 : current + 1;
    options.forEach((option, index) => { option.tabIndex = index === next ? 0 : -1; });
    options[next].focus();
  });
  document.addEventListener("pointerdown", (event) => {
    if (event.button !== 2) state.inspectorCycle = null;
    if (!profileMenu.hidden && !profilePicker.contains(event.target)) closeProfileMenu();
    if (state.inspectorPopup && !inspector.contains(event.target)) {
      const target = event.target;
      const preserveCycle = event.button === 2
        && target instanceof Element
        && Boolean(target.closest(".overlay"));
      closeInspectorPopup(preserveCycle);
    }
  }, true);
  profilePicker.addEventListener("focusout", (event) => {
    if (!profileMenu.hidden && !profilePicker.contains(event.relatedTarget)) closeProfileMenu();
  });
  profileAdd.addEventListener("click", openProfileCreator);
  profileImport.addEventListener("click", () => {
    if (!state.config) return;
    closeProfileMenu();
    closeProfileCreator();
    ipc({ type: "import-profile" });
  });
  profileExport.addEventListener("click", () => {
    flushEditingChanges();
    const profile = activeProfile();
    if (!profile) return;
    closeProfileMenu();
    closeProfileCreator();
    ipc({ type: "export-profile", profile });
  });
  $("import-tosu-layout").addEventListener("click", () => {
    if (!state.config) return;
    closeSettingsModal(false);
    closeProfileMenu();
    closeProfileCreator();
    ipc({ type: "import-tosu-layout" });
  });
  profileCreate.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!state.config) return;
    const name = profileName.value.trim();
    if (!name) {
      toast("Profile name cannot be empty", true);
      profileName.focus();
      profileName.select();
      return;
    }
    const profile = { id: uniqueProfileId(), name, overlays: [] };
    flushEditingChanges();
    const before = historySnapshot();
    replaceSelection([]);
    state.config.profiles.push(profile);
    state.config.active_profile = profile.id;
    commitHistory(before);
    closeProfileCreator();
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
    (profileSelect.getClientRects().length ? profileSelect : profileAdd).focus();
  });
  profileCreateCancel.addEventListener("click", (event) => {
    event.preventDefault();
    closeProfileCreator(true);
  });
  profileDelete.addEventListener("click", openProfileDeleteConfirm);
  profileDeleteAccept.addEventListener("click", deleteConfirmedProfile);
  profileDeleteCancel.addEventListener("click", () => closeProfileDeleteConfirm(true));
  profilePosition.addEventListener("input", () => {
    const value = Number(profilePosition.value);
    const valid = profilePosition.value.trim() !== "" && Number.isInteger(value)
      && value >= 1 && value <= (state.config ? state.config.profiles.length : 0);
    if (valid) profilePosition.removeAttribute("aria-invalid");
    else profilePosition.setAttribute("aria-invalid", "true");
  });
  profilePosition.addEventListener("change", commitProfilePosition);
  profilePosition.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      if (!event.repeat) commitProfilePosition();
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      const profile = activeProfile();
      profilePosition.value = profile ? String(state.config.profiles.indexOf(profile) + 1) : "";
      profilePosition.removeAttribute("aria-invalid");
      profilePosition.blur();
    }
  });
  undoButton.addEventListener("click", undoEdit);
  redoButton.addEventListener("click", redoEdit);
  refreshSelectedButton.addEventListener("click", refreshSelectedOverlays);
  refreshAllButton.addEventListener("click", refreshAllOverlays);

  function commitTitle() {
    const overlay = selectedOverlay();
    if (!overlay) return;
    const next = nameField.value.trim();
    const current = typeof overlay.name === "string" ? overlay.name : "";
    if (!next) {
      setError("Overlay title cannot be empty");
      nameField.value = current || overlayDetails(overlay).name;
      return;
    }
    if (!current && next === overlayDetails(overlay).name) return;
    if (next === current) return;
    beginInspectorEdit();
    overlay.name = next;
    commitInspectorEdit();
    scheduleSave();
    requestRender("stageStructure", "inspector");
  }

  function commitUrl() {
    const overlay = selectedOverlay();
    if (!overlay || !overlay.url) return;
    const raw = urlField.value.trim();
    const source = parseSource(raw, ["http:", "https:", "vosulay-overlay:"]);
    if (!raw || !source) {
      setError(!raw ? "Overlay URL cannot be empty" : "Overlay URL must use http, https, or vosulay-overlay");
      urlField.value = overlay.url;
      return;
    }
    if (urlField.value !== source.href) urlField.value = source.href;
    if (source.href === overlay.url) return;
    beginInspectorEdit();
    overlay.url = source.href;
    commitInspectorEdit();
    scheduleSave(true);
    requestRender("stageStructure", "inspector");
  }

  function commitInspectorOnEnter(event, commit) {
    if (event.key !== "Enter") return;
    event.preventDefault();
    event.stopPropagation();
    if (!event.repeat) commit();
  }

  for (const input of [nameField, urlField]) {
    input.addEventListener("focus", beginInspectorEdit);
    input.addEventListener("blur", endInspectorEdit);
  }
  nameField.addEventListener("change", commitTitle);
  nameField.addEventListener("keydown", (event) => commitInspectorOnEnter(event, commitTitle));
  urlField.addEventListener("change", commitUrl);
  urlField.addEventListener("keydown", (event) => commitInspectorOnEnter(event, commitUrl));

  function commitInspectorNumber(name, input) {
    const overlay = selectedOverlay();
    if (!overlay) return;
    const raw = input.value.trim();
    const value = raw === "" ? NaN : Number(raw);
    if (!Number.isFinite(value)) {
      const previousPosition = positionFocusValues.get(input);
      if (Number.isFinite(previousPosition)) {
        applyInspectorFieldValue(name, overlay, previousPosition);
        requestRender("stageGeometry", "inspector");
      }
      positionFocusValues.delete(input);
      input.value = inspectorFieldValue(name, overlay);
      showFieldWarning(input, "Enter a number.");
      return;
    }

    const { minimum, maximum } = inspectorFieldBounds(input);
    positionFocusValues.delete(input);
    const adjusted = clamp(value, minimum, maximum);
    applyInspectorFieldValue(name, overlay, adjusted);
    const positionAdjusted = (name === "left" || name === "top") && overlay[name] !== value;
    input.value = inspectorFieldValue(name, overlay);
    scheduleSave();
    requestRender("stageGeometry", "inspector");
    if (value < minimum) {
      showFieldWarning(input, `Value must be at least ${input.min}. Adjusted to ${input.min}.`);
    } else if (value > maximum) {
      showFieldWarning(input, `Value must be at most ${input.max}. Adjusted to ${input.max}.`);
    } else if (positionAdjusted) {
      showFieldWarning(input, `Value was adjusted to ${input.value} to keep the overlay in view.`);
    } else {
      clearFieldWarning(input);
    }
  }

  for (const [name, input] of Object.entries(fields)) {
    input.addEventListener("focus", () => {
      beginInspectorEdit();
      if (name !== "left" && name !== "top") return;
      const overlay = selectedOverlay();
      if (overlay) positionFocusValues.set(input, overlay[name]);
    });
    input.addEventListener("input", () => {
      clearFieldWarning(input);
      const overlay = selectedOverlay();
      if (!overlay || input.value.trim() === "") return;
      const value = Number(input.value);
      const { minimum, maximum } = inspectorFieldBounds(input);
      if (!Number.isFinite(value) || value < minimum || value > maximum) return;
      beginInspectorEdit();
      const positionField = name === "left" || name === "top";
      if (positionField) {
        overlay[name] = value;
        const root = overlayRoots.get(overlay);
        if (root) applyOverlayStyle(root, overlay);
        commitInspectorEdit();
        updatePanelAvoidance();
        return;
      }
      applyInspectorFieldValue(name, overlay, value);
      commitInspectorEdit();
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
    input.addEventListener("blur", () => {
      commitInspectorNumber(name, input);
      endInspectorEdit();
    });
  }

  function showSettingsError(message) {
    settingsError.textContent = message;
    settingsError.hidden = !message;
  }

  // Empty means the default loopback listener; null marks an invalid address
  // so the dialog reports it instead of silently replacing it.
  function normalizedRemoteListenHost(value) {
    const trimmed = String(value || "").trim();
    const unbracketed = trimmed.startsWith("[") && trimmed.endsWith("]")
      ? trimmed.slice(1, -1)
      : trimmed;
    if (!unbracketed) return DEFAULT_REMOTE_LISTEN_HOST;
    if (/^(\d{1,3}\.){3}\d{1,3}$/.test(unbracketed)
      && unbracketed.split(".").every((part) => Number(part) <= 255 && String(Number(part)) === part)) {
      return unbracketed;
    }
    if (unbracketed.includes(":") && /^[0-9a-fA-F:]+$/.test(unbracketed)) return unbracketed;
    return null;
  }

  function normalizedRemoteListenPort(value) {
    const raw = String(value ?? "").trim();
    if (!raw) return DEFAULT_REMOTE_LISTEN_PORT;
    const port = Number(raw);
    return Number.isInteger(port) && port >= 1 && port <= 65535 ? port : null;
  }

  function renderSettingsDraft() {
    if (!state.settingsDraft) return;
    keepOverlaysInView.checked = state.settingsDraft.always_keep_overlays_in_view;
    allowUnrestrictedSharedArrayBuffer.checked = state.settingsDraft.allow_unrestricted_shared_array_buffer;
    for (const radio of fpsLimitModes) radio.checked = radio.value === state.settingsDraft.fps_mode;
    if (fpsLimit.value !== state.settingsDraft.fps_value) fpsLimit.value = state.settingsDraft.fps_value;
    fpsLimit.disabled = state.settingsDraft.fps_mode !== "fixed";
    allowedIps.value = state.settingsDraft.allowed_ips;
    remoteListenHost.value = state.settingsDraft.remote_listen_host;
    remoteListenPort.value = state.settingsDraft.remote_listen_port;
    for (const button of hotkeyList.querySelectorAll(".hotkey-input")) {
      const key = button.dataset.hotkey;
      button.textContent = state.settingsDraft.hotkeys[key] || DEFAULT_HOTKEYS[key] || "";
      button.classList.toggle("recording", state.recordingHotkey === key);
      button.setAttribute("aria-pressed", String(state.recordingHotkey === key));
    }
  }

  function closeSettingsModal(restoreFocus = true) {
    if (settingsModal.hidden) return;
    settingsModal.hidden = true;
    state.settingsDraft = null;
    state.recordingHotkey = "";
    showSettingsError("");
    if (restoreFocus && openSettings.getClientRects().length) openSettings.focus();
  }

  function openSettingsModal() {
    if (!state.config) return;
    closeUrlModal(false);
    closeProfileMenu();
    closeProfileCreator();
    closeProfileDeleteConfirm();
    closeInspectorPopup();
    const settings = state.config.settings;
    state.settingsDraft = {
      always_keep_overlays_in_view: settings.always_keep_overlays_in_view,
      fps_mode: settings.fps_limit === 0 ? "auto" : "fixed",
      fps_value: settings.fps_limit ? String(settings.fps_limit) : "",
      allow_unrestricted_shared_array_buffer: settings.allow_unrestricted_shared_array_buffer,
      allowed_ips: settings.allowed_ips,
      remote_listen_host: settings.remote_listen_host,
      remote_listen_port: String(settings.remote_listen_port),
      hotkeys: { ...settings.hotkeys }
    };
    state.recordingHotkey = "";
    showSettingsError("");
    for (const input of [fpsLimit, remoteListenHost, remoteListenPort]) input.removeAttribute("aria-invalid");
    renderSettingsDraft();
    settingsModal.hidden = false;
    keepOverlaysInView.focus();
  }

  function eventHotkey(event) {
    if (["Control", "Alt", "Shift", "Meta"].includes(event.key)) return "";
    const key = event.key === " " ? "Space"
      : event.key === "+" ? "Plus"
        : event.key === "-" ? "Minus"
          : event.key.length === 1 ? event.key.toUpperCase()
            : event.key;
    const parts = [];
    if (event.ctrlKey) parts.push("Ctrl");
    if (event.altKey) parts.push("Alt");
    if (event.shiftKey) parts.push("Shift");
    if (event.metaKey) parts.push("Meta");
    parts.push(key);
    return canonicalHotkey(parts.join("+"), "");
  }

  function copySelectedPlacements() {
    const profile = activeProfile();
    if (!profile || !state.selected.size) return;
    state.copiedPlacements = profile.overlays
      .filter((overlay) => state.selected.has(overlay))
      .map((overlay) => ({ ...overlay }));
  }

  function pasteCopiedPlacements() {
    const profile = activeProfile();
    if (!profile || !state.copiedPlacements.length) return;
    const before = historySnapshot();
    const pasted = state.copiedPlacements.map((overlay) => normalizeOverlay({ ...overlay }));
    profile.overlays.push(...pasted);
    replaceSelection(pasted);
    commitHistory(before);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  openSettings.addEventListener("click", openSettingsModal);
  settingsCancel.addEventListener("click", () => closeSettingsModal());
  keepOverlaysInView.addEventListener("change", () => {
    if (state.settingsDraft) state.settingsDraft.always_keep_overlays_in_view = keepOverlaysInView.checked;
  });
  allowUnrestrictedSharedArrayBuffer.addEventListener("change", () => {
    if (state.settingsDraft) state.settingsDraft.allow_unrestricted_shared_array_buffer = allowUnrestrictedSharedArrayBuffer.checked;
  });
  for (const radio of fpsLimitModes) {
    radio.addEventListener("change", () => {
      if (!state.settingsDraft || !radio.checked) return;
      state.settingsDraft.fps_mode = radio.value;
      fpsLimit.disabled = radio.value !== "fixed";
      fpsLimit.removeAttribute("aria-invalid");
      showSettingsError("");
    });
  }
  fpsLimit.addEventListener("input", () => {
    if (!state.settingsDraft) return;
    state.settingsDraft.fps_value = fpsLimit.value;
    fpsLimit.removeAttribute("aria-invalid");
  });
  allowedIps.addEventListener("input", () => {
    if (state.settingsDraft) state.settingsDraft.allowed_ips = allowedIps.value;
  });
  remoteListenHost.addEventListener("input", () => {
    if (!state.settingsDraft) return;
    state.settingsDraft.remote_listen_host = remoteListenHost.value;
    remoteListenHost.removeAttribute("aria-invalid");
  });
  remoteListenPort.addEventListener("input", () => {
    if (!state.settingsDraft) return;
    state.settingsDraft.remote_listen_port = remoteListenPort.value;
    remoteListenPort.removeAttribute("aria-invalid");
  });

  function rejectSettingsField(input, message) {
    input.setAttribute("aria-invalid", "true");
    showSettingsError(message);
    input.focus();
  }
  settingsResetHotkeys.addEventListener("click", () => {
    if (!state.settingsDraft) return;
    state.settingsDraft.hotkeys = { ...DEFAULT_HOTKEYS };
    state.recordingHotkey = "";
    showSettingsError("");
    renderSettingsDraft();
  });
  hotkeyList.addEventListener("click", (event) => {
    const button = event.target.closest(".hotkey-input");
    if (!button || !state.settingsDraft || !HOTKEY_KEYS.includes(button.dataset.hotkey)) return;
    state.recordingHotkey = button.dataset.hotkey;
    showSettingsError("");
    renderSettingsDraft();
    button.focus();
  });
  hotkeyList.addEventListener("keydown", (event) => {
    if (!state.settingsDraft || !state.recordingHotkey) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      state.recordingHotkey = "";
      renderSettingsDraft();
      return;
    }
    const shortcut = eventHotkey(event);
    if (!shortcut) return;
    event.preventDefault();
    event.stopPropagation();
    if (RESERVED_HOTKEYS.has(shortcut)) {
      showSettingsError(`${shortcut} is reserved for built-in overlay controls.`);
      return;
    }
    const duplicate = HOTKEY_KEYS.find((key) => key !== state.recordingHotkey
      && state.settingsDraft.hotkeys[key] === shortcut);
    if (duplicate) {
      showSettingsError(`${shortcut} is already assigned.`);
      return;
    }
    state.settingsDraft.hotkeys[state.recordingHotkey] = shortcut;
    state.recordingHotkey = "";
    showSettingsError("");
    renderSettingsDraft();
  });
  settingsForm.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!state.settingsDraft) return;
    const draft = state.settingsDraft;
    let fpsValue = 0;
    if (draft.fps_mode === "fixed") {
      const raw = draft.fps_value.trim();
      fpsValue = Number(raw);
      if (!raw || !Number.isInteger(fpsValue) || fpsValue < 1 || fpsValue > 1000000) {
        rejectSettingsField(fpsLimit, "Fixed FPS limit must be a whole number from 1 to 1000000.");
        return;
      }
    }
    const listenHost = normalizedRemoteListenHost(draft.remote_listen_host);
    if (listenHost === null) {
      rejectSettingsField(remoteListenHost, "Listener address must be an IPv4 or IPv6 address, such as 127.0.0.1 or 0.0.0.0.");
      return;
    }
    const listenPort = normalizedRemoteListenPort(draft.remote_listen_port);
    if (listenPort === null) {
      rejectSettingsField(remoteListenPort, "Listener port must be a whole number from 1 to 65535.");
      return;
    }
    const shortcuts = HOTKEY_KEYS.map((key) => draft.hotkeys[key]);
    if (new Set(shortcuts).size !== shortcuts.length) {
      showSettingsError("Each action must have a unique shortcut.");
      return;
    }
    state.config.settings = {
      always_keep_overlays_in_view: draft.always_keep_overlays_in_view,
      fps_limit: fpsValue,
      allow_unrestricted_shared_array_buffer: draft.allow_unrestricted_shared_array_buffer,
      allowed_ips: draft.allowed_ips,
      remote_listen_host: listenHost,
      remote_listen_port: listenPort,
      hotkeys: normalizeHotkeys(draft.hotkeys)
    };
    closeSettingsModal();
    closeInspectorPopup();
    const overlay = selectedOverlay();
    if (overlay) {
      const before = historySnapshot();
      clampOverlay(overlay);
      commitHistory(before);
    }
    scheduleSave(true);
    requestRender("stageGeometry", "inspector", "chrome");
  });
  settingsModal.addEventListener("keydown", (event) => {
    if (event.key !== "Tab") return;
    const focusable = [...settingsModal.querySelectorAll("button, input, select, textarea")]
      .filter((element) => !element.disabled && element.getClientRects().length);
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  });

  $("remove").addEventListener("click", removeSelected);
  $("reset").addEventListener("click", () => {
    const overlay = selectedOverlay();
    if (!overlay) return;
    const before = historySnapshot();
    const counter = findCounter(overlay.counter_id);
    overlay.width = Math.max(10, finite(counter && counter.width, 500));
    overlay.height = Math.max(10, finite(counter && counter.height, 300));
    overlay.scale = 1;
    clampOverlay(overlay);
    commitHistory(before);
    scheduleSave(true);
    requestRender("stageGeometry", "inspector");
  });
  $("settings").addEventListener("click", () => {
    const overlay = selectedOverlay();
    const counter = overlay && !overlay.url && findCounter(overlay.counter_id);
    if (counter && counter.has_settings) ipc({ type: "open-settings", counter_id: counter.id });
  });
  addUrl.addEventListener("click", openUrlModal);
  urlCancel.addEventListener("click", () => closeUrlModal());
  urlForm.addEventListener("submit", (event) => {
    event.preventDefault();
    const name = urlName.value.trim();
    if (!name) {
      setError("Overlay title cannot be empty");
      urlName.focus();
      urlName.select();
      return;
    }
    const raw = urlInput.value.trim();
    const source = parseSource(raw, ["http:", "https:"]);
    if (!raw || !source) {
      setError(!raw ? "Overlay URL cannot be empty" : "Overlay URL must be a valid http or https URL");
      urlInput.focus();
      urlInput.select();
      return;
    }
    addCustomOverlay(source, name);
    closeUrlModal();
  });
  $("add-html-file").addEventListener("click", () => ipc({ type: "choose-overlay-file" }));

  $("refresh").addEventListener("click", () => {
    status.textContent = "Refreshing…";
    ipc({ type: "refresh-counters" });
  });

  $("done").addEventListener("click", () => setEditing(false));

  const TEXT_INPUT_TYPES = new Set(["text", "number", "url", "search", "email", "password", "tel"]);

  // Only text entry keeps native shortcuts (undo/redo, copy/paste); checkboxes,
  // radios and buttons still let editor hotkeys through.
  function isEditableTarget(target) {
    if (!(target instanceof Element)) return false;
    if (target.isContentEditable || target.closest("textarea, select")) return true;
    const input = target.closest("input");
    return Boolean(input) && TEXT_INPUT_TYPES.has(input.type);
  }

  function menuOrModalOpen() {
    return !settingsModal.hidden || !urlModal.hidden || !profileMenu.hidden
      || !profileCreate.hidden || !profileDeleteConfirm.hidden || state.inspectorPopup;
  }

  function runHotkeyAction(action) {
    if (action === "copy_overlay") copySelectedPlacements();
    else if (action === "paste_overlay") pasteCopiedPlacements();
    else if (action === "undo") undoEdit();
    else if (action === "redo") redoEdit();
    else if (action === "refresh_selected_overlay") refreshSelectedOverlays();
    else if (action === "refresh_all_overlays") refreshAllOverlays();
    else {
      const number = Number(action.slice("profile_".length));
      const profile = state.config.profiles[number === 0 ? 9 : number - 1];
      if (profile) activateProfile(profile.id);
    }
  }

  document.addEventListener("wheel", (event) => {
    if (event.ctrlKey) event.preventDefault();
  }, { capture: true, passive: false });
  document.addEventListener("keydown", (event) => {
    // Browser reload would discard the editor page and any unsaved edits; the
    // refresh actions reload overlay frames instead.
    const reloadKey = event.key === "F5" || event.code === "F5"
      || ((event.ctrlKey || event.metaKey) && !event.altKey
        && (event.code === "KeyR" || event.key.toLowerCase() === "r"));
    if (reloadKey) {
      event.preventDefault();
      return;
    }
    if (!event.ctrlKey || event.altKey || event.metaKey) return;
    const zoomKey = ["+", "-", "=", "0"].includes(event.key)
      || ["Equal", "Minus", "NumpadAdd", "NumpadSubtract", "Digit0", "Numpad0"].includes(event.code);
    if (zoomKey) event.preventDefault();
  }, true);

  window.addEventListener("keydown", (event) => {
    if (event.key === "Tab") releaseToolbarHold(NaN, NaN);
    const editable = isEditableTarget(event.target);
    if (state.editing && !editable && !menuOrModalOpen() && state.config) {
      const shortcut = eventHotkey(event);
      const action = shortcut && HOTKEY_KEYS.find((key) => state.config.settings.hotkeys[key] === shortcut);
      if (action) {
        event.preventDefault();
        if (!event.repeat || REPEATABLE_HOTKEYS.has(action)) runHotkeyAction(action);
        return;
      }
    }

    if (event.key === "Delete") {
      if (event.repeat || menuOrModalOpen() || editable || !state.editing || !selectedOverlay()) return;
      removeSelected();
      event.preventDefault();
      return;
    }

    if (event.key !== "Escape") return;
    if (event.repeat) {
      if (menuOrModalOpen() || state.editing) event.preventDefault();
      return;
    }
    if (!settingsModal.hidden) {
      event.preventDefault();
      closeSettingsModal();
    } else if (!urlModal.hidden) {
      event.preventDefault();
      closeUrlModal();
    } else if (!profileMenu.hidden) {
      event.preventDefault();
      closeProfileMenu(true);
    } else if (!profileDeleteConfirm.hidden) {
      event.preventDefault();
      closeProfileDeleteConfirm(true);
    } else if (!profileCreate.hidden) {
      event.preventDefault();
      closeProfileCreator(true);
    } else if (state.inspectorPopup) {
      event.preventDefault();
      closeInspectorPopup();
    } else if (state.editing) {
      event.preventDefault();
      setEditing(false);
    }
  });
  window.addEventListener("resize", () => {
    if (state.resizeFrame) return;
    state.resizeFrame = requestAnimationFrame(() => {
      state.resizeFrame = 0;
      updatePanelAvoidance();
      if (!profileMenu.hidden && profilePicker.getClientRects().length === 0) closeProfileMenu();
      const overlay = state.editing ? selectedOverlay() : null;
      if (!overlay) return;
      const before = `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}\n${overlay.rotation}`;
      clampOverlay(overlay);
      const after = `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}\n${overlay.rotation}`;
      if (state.inspectorPopup) requestRender("inspector");
      if (before === after) return;
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
  });

  function receive(message) {
    try {
      const envelope = typeof message === "string" ? JSON.parse(message) : message;
      if (!envelope || typeof envelope !== "object") throw new Error("Invalid message from overlay host");
      if (envelope.type === "config") {
        const revision = Number.isSafeInteger(envelope.revision) && envelope.revision >= 0
          ? envelope.revision
          : null;
        if (revision !== null && (state.pendingSaveId || state.saveInFlightId)) {
          if (!state.deferredConfig || revision >= state.deferredConfig.revision) {
            state.deferredConfig = { config: envelope.config, revision };
          }
          if (state.retryRequired) state.hostRevision = revision;
          else flushPendingSave();
          return;
        }
        applyHostConfig(envelope);
      } else if (envelope.type === "saved") {
        if (Number.isSafeInteger(envelope.revision) && envelope.revision >= 0
            && (state.hostRevision === null || envelope.revision >= state.hostRevision)) {
          state.hostRevision = envelope.revision;
        }
        if (envelope.save_id === state.saveInFlightId) state.saveInFlightId = "";
        state.retryRequired = false;
        if (state.latestSaveId && envelope.save_id === state.latestSaveId) {
          if (state.saveErrorMessage && state.error === state.saveErrorMessage) {
            state.error = null;
            state.saveErrorMessage = "";
            requestRender("chrome");
          }
          status.textContent = "Saved";
        }
        if (state.pendingSaveId) {
          flushPendingSave();
        } else if (state.deferredConfig) {
          const deferred = state.deferredConfig;
          state.deferredConfig = null;
          if (state.hostRevision === null || deferred.revision > state.hostRevision) applyHostConfig(deferred);
        }
        settleFlushRequests(null);
      } else if (envelope.type === "save-error") {
        const message = typeof envelope.message === "string" ? envelope.message : "vosulay could not save the configuration";
        if (envelope.save_id === state.saveInFlightId) state.saveInFlightId = "";
        clearTimeout(state.saveTimer);
        state.saveTimer = 0;
        let recovery = envelope;
        if (state.deferredConfig
            && (!Number.isSafeInteger(envelope.revision) || state.deferredConfig.revision > envelope.revision)) {
          recovery = state.deferredConfig;
        }
        state.deferredConfig = null;
        const hasNewerLocalEdit = Boolean(state.pendingSaveId);
        const preserveLocal = envelope.code === "conflict" || hasNewerLocalEdit;
        if (Number.isSafeInteger(recovery.revision) && recovery.revision >= 0) {
          state.hostRevision = recovery.revision;
        }
        if (preserveLocal) {
          if (!state.pendingSaveId) {
            const retryId = String(++state.saveRevision);
            state.latestSaveId = retryId;
            state.pendingSaveId = retryId;
          }
          state.retryRequired = true;
        } else {
          state.pendingSaveId = "";
          if (recovery.config !== undefined) applyHostConfig(recovery);
        }
        state.saveErrorMessage = envelope.code === "conflict"
          ? `${message}. Local edits remain onscreen but unsaved; edit again to retry, or restart vosulay to accept the saved configuration.`
          : hasNewerLocalEdit
            ? `${message}. Newer local edits remain onscreen but unsaved; edit again to retry.`
            : message;
        setError(state.saveErrorMessage);
        settleFlushRequests(state.saveErrorMessage);
      } else if (envelope.type === "flush-save") {
        if (typeof envelope.request_id !== "string" || !envelope.request_id) throw new Error("Invalid save flush request");
        if (!state.flushRequestIds.includes(envelope.request_id)) state.flushRequestIds.push(envelope.request_id);
        state.retryRequired = false;
        flushPendingSave();
        settleFlushRequests(null);
      } else if (envelope.type === "counters") {
        const value = envelope.counters !== undefined ? envelope.counters : envelope.payload;
        state.counters = Array.isArray(value) ? value.filter((counter) => counter && typeof counter.id === "string").map((counter) => ({
          id: counter.id,
          name: safeId(counter.name, counter.id),
          author: typeof counter.author === "string" ? counter.author : "",
          version: typeof counter.version === "string" ? counter.version : "",
          width: Math.max(10, finite(counter.width, 500)),
          height: Math.max(10, finite(counter.height, 300)),
          has_settings: Boolean(counter.has_settings),
          url: typeof counter.url === "string" ? counter.url : "about:blank"
        })) : [];
        counterById.clear();
        for (const counter of state.counters) {
          if (!counterById.has(counter.id)) counterById.set(counter.id, counter);
        }
        state.error = null;
        status.textContent = "Catalog refreshed";
        requestRender("stageStructure", "catalog", "inspector");
      } else if (envelope.type === "profile-exported") {
        status.textContent = "Profile exported";
        toast("Profile exported");
      } else if (envelope.type === "tosu-layout-imported") {
        toast(`Imported ${envelope.profiles} tosu profile${envelope.profiles === 1 ? "" : "s"}`);
      } else if (envelope.type === "profile-import") {
        prepareProfileImport(envelope.profile);
      } else if (envelope.type === "profile-import-authorized") {
        commitProfileImport(envelope.profile);
      } else if (envelope.type === "overlay-file") {
        const source = parseSource(envelope.url, ["vosulay-overlay:"]);
        if (!source) {
          setError("HTML overlay must be a valid internal overlay URL");
          return;
        }
        addCustomOverlay(source, safeId(envelope.name, deriveUrlName(source) || "HTML overlay"));
      } else if (envelope.type === "editing") {
        const value = envelope.editing !== undefined ? envelope.editing : envelope.payload;
        setEditing(Boolean(value), false);
      } else if (envelope.type === "toggle-interaction") {
        if (state.editing && !menuOrModalOpen() && !isEditableTarget(document.activeElement)) {
          state.interacting = !state.interacting;
          requestRender("chrome");
        }
      } else if (envelope.type === "error") {
        const value = envelope.message !== undefined ? envelope.message : envelope.error !== undefined ? envelope.error : envelope.payload;
        setError(typeof value === "string" ? value : "vosu could not refresh counters");
      }
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    }
  }

  window.__VOSULAY_RECEIVE__ = receive;
  window.vosulay = Object.freeze({ receive });

  requestRender();
  ipc({ type: "ready" });
})();
