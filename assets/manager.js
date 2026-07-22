(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const stage = $("stage");
  const editor = $("editor");
  const inspector = $("inspector");
  const sidePanels = document.querySelectorAll(".sidebar, #inspector");
  const sidePanelClearance = 24;
  const profileSelect = $("profile-select");
  const profileSelectLabel = $("profile-select-label");
  const profileMenu = $("profile-menu");
  const profilePicker = profileSelect.closest(".profile-picker");
  const profileAdd = $("profile-add");
  const profileActions = $("profile-actions");
  const profileImport = $("profile-import");
  const profileExport = $("profile-export");
  const profileCreate = $("profile-create");
  const profileName = $("profile-name");
  const profileCreateCancel = $("profile-create-cancel");
  const catalog = $("catalog");
  const status = $("status");
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
  const openSettings = $("open-settings");
  const settingsModal = $("settings-modal");
  const settingsForm = $("settings-form");
  const rightClickSettings = $("right-click-settings");
  const keepOverlaysInView = $("keep-overlays-in-view");
  const hotkeyList = $("hotkey-list");
  const settingsError = $("settings-error");
  const settingsResetHotkeys = $("settings-reset-hotkeys");
  const settingsCancel = $("settings-cancel");
  const selectionBox = $("selection-box");
  const HOTKEY_KEYS = [
    ...Array.from({ length: 10 }, (_, index) => `profile_${index}`),
    "copy_overlay",
    "paste_overlay"
  ];
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
    paste_overlay: "Ctrl+V"
  });

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
    saveTimer: 0,
    saveRevision: 0,
    latestSaveId: "",
    pendingSaveId: "",
    renderPending: false,
    pointer: null,
    resizeFrame: 0,
    error: null
  };

  const outboundQueue = [];

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
    return Object.fromEntries(HOTKEY_KEYS.map((key) => [
      key,
      canonicalHotkey(source[key], DEFAULT_HOTKEYS[key])
    ]));
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
      settings: {
        right_click_overlay_settings_menu: Boolean(sourceSettings.right_click_overlay_settings_menu),
        always_keep_overlays_in_view: Boolean(sourceSettings.always_keep_overlays_in_view),
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

  function updatePanelAvoidance() {
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
      const panelRect = panel.getBoundingClientRect();
      const avoids = overlayRect.left <= panelRect.right + sidePanelClearance
        && overlayRect.right >= panelRect.left - sidePanelClearance
        && overlayRect.top <= panelRect.bottom + sidePanelClearance
        && overlayRect.bottom >= panelRect.top - sidePanelClearance;
      panel.classList.toggle("avoids-selected-overlay", avoids);
    }
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
    const saveId = state.pendingSaveId;
    state.pendingSaveId = "";
    if (state.saveTimer) {
      clearTimeout(state.saveTimer);
      state.saveTimer = 0;
    }
    ipc({ type: "save-config", save_id: saveId, config: state.config });
  }

  function scheduleSave(immediate = false) {
    if (!state.config) return;
    const saveId = String(++state.saveRevision);
    state.latestSaveId = saveId;
    state.pendingSaveId = saveId;
    clearTimeout(state.saveTimer);
    status.textContent = "Saving…";
    if (immediate) flushPendingSave();
    else state.saveTimer = window.setTimeout(flushPendingSave, 240);
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
    const root = makeElement("div", `overlay${details.available ? "" : " missing"}${state.selected.has(overlay) ? " selected" : ""}`);
    root.dataset.counterId = overlay.counter_id;
    root._vosulayOverlay = overlay;
    root._vosulaySourceIdentity = `${details.available ? "available" : "missing"}\n${details.url || ""}\n${overlay.url ? "custom" : "counter"}`;
    applyOverlayStyle(root, overlay);

    if (details.available) {
      const frame = makeElement("iframe", "overlay-frame");
      frame.src = details.url;
      frame.title = details.name;
      frame.setAttribute("allow", "autoplay");
      if (overlay.url) {
        frame.setAttribute("sandbox", "allow-scripts allow-same-origin");
      }
      frame.setAttribute("loading", "eager");
      root.append(frame);
    } else {
      const missing = makeElement("div", "missing-card");
      missing.append(makeElement("strong", "", "Counter unavailable"));
      missing.append(makeElement("small", "", overlay.counter_id));
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
        const identity = `${details.available ? "available" : "missing"}\n${details.url || ""}\n${overlay.url ? "custom" : "counter"}`;
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
        clampOverlay(overlay);
        const root = overlayRoots.get(overlay);
        if (!root) continue;
        applyOverlayStyle(root, overlay);
        root.classList.toggle("selected", state.selected.has(overlay));
        const soleSelected = state.selected.size === 1 && state.selected.has(overlay);
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
      return;
    }
    const current = activeProfile();
    profileSelectLabel.textContent = current ? current.name : "";
    for (const profile of state.config.profiles) {
      const option = makeElement("button", "profile-option", profile.name);
      const active = profile === current;
      option.type = "button";
      option.dataset.profileId = profile.id;
      option.setAttribute("role", "menuitemradio");
      option.setAttribute("aria-checked", String(active));
      option.tabIndex = active ? 0 : -1;
      profileMenu.append(option);
    }
    $("profile-delete").disabled = state.config.profiles.length <= 1;
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
      catalog.append(makeElement("div", "catalog-empty", "No installed counters found. Browse available counters to install one."));
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
    const contextual = Boolean(state.config?.settings.right_click_overlay_settings_menu);
    document.body.classList.toggle("context-inspector", contextual && state.inspectorPopup);
    const warningContext = state.editing ? overlay : null;
    if (inspectorWarningContext !== warningContext) {
      inspectorWarningContext = warningContext;
      clearInspectorWarnings();
    }
    inspector.hidden = !state.editing || !overlay || (contextual && !state.inspectorPopup);
    if (inspector.hidden) return;
    if (contextual && state.inspectorPoint) {
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
    renderInspectorNumbers(overlay);
    $("settings").disabled = custom || !counter || !counter.has_settings;
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
    updatePanelAvoidance();
  }

  function closeInspectorPopup() {
    if (!state.inspectorPopup && !state.inspectorPoint) return;
    state.inspectorPopup = false;
    state.inspectorPoint = null;
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
    replaceSelection([]);
    state.config.profiles.push(profile);
    state.config.active_profile = profile.id;
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
    status.textContent = "Profile imported";
    toast(`Imported “${profile.name}”`);
  }

  function addCounter(counter) {
    const profile = activeProfile();
    if (!profile) return;
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
    clampOverlay(overlay);
    profile.overlays.push(overlay);
    selectOverlay(overlay);
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
    clampOverlay(overlay);
    profile.overlays.push(overlay);
    selectOverlay(overlay);
    scheduleSave();
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function removeSelected() {
    const profile = activeProfile();
    const overlay = selectedOverlay();
    if (!profile || !overlay) return;
    const index = profile.overlays.indexOf(overlay);
    if (index < 0) return;
    replaceSelection([]);
    profile.overlays.splice(index, 1);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function refreshSelectedOverlay() {
    const overlay = selectedOverlay();
    const frame = overlay && overlayRoots.get(overlay)?.querySelector(".overlay-frame");
    if (!frame) return;
    frame.src = frame.src;
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
    profileName.value = `Profile ${state.config.profiles.length + 1}`;
    profileActions.hidden = true;
    profileCreate.hidden = false;
    profileName.focus();
    profileName.select();
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
    if (inspector.contains(document.activeElement)) document.activeElement.blur();
    root.setPointerCapture(event.pointerId);
    const angle = overlay.rotation * Math.PI / 180;
    const placements = [...state.selected].map((placement) => ({
      overlay: placement,
      left: placement.left,
      top: placement.top
    }));
    const movementBounds = placements.reduce((bounds, placement) => {
      const range = overlayPositionRange(placement.overlay);
      bounds.minX = Math.max(bounds.minX, range.minLeft - placement.left);
      bounds.maxX = Math.min(bounds.maxX, range.maxLeft - placement.left);
      bounds.minY = Math.max(bounds.minY, range.minTop - placement.top);
      bounds.maxY = Math.min(bounds.maxY, range.maxTop - placement.top);
      return bounds;
    }, { minX: -Infinity, maxX: Infinity, minY: -Infinity, maxY: Infinity });
    state.pointer = {
      kind: "overlay",
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
      scheduleSave(true);
    }
    requestRender("stageGeometry", "inspector");
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
  stage.addEventListener("contextmenu", (event) => {
    if (!state.editing) return;
    const root = event.target.closest(".overlay");
    if (!root) return;
    event.preventDefault();
    const contextual = Boolean(state.config?.settings.right_click_overlay_settings_menu);
    selectOverlay(root._vosulayOverlay, { keepPopup: contextual });
    if (!contextual) return;
    state.inspectorPopup = true;
    state.inspectorPoint = { x: event.clientX, y: event.clientY };
    requestRender("inspector");
  });
  stage.addEventListener("wheel", (event) => {
    if (event.ctrlKey || !state.editing) return;
    const root = event.target.closest(".overlay");
    if (!root) return;
    event.preventDefault();
    selectOverlay(root._vosulayOverlay);
    const overlay = selectedOverlay();
    if (!overlay) return;
    overlay.z_index = clamp(overlay.z_index + (event.deltaY < 0 ? 1 : -1), 0, 999);
    applyOverlayStyle(root, overlay);
    renderInspectorNumbers(overlay, ["z_index"]);
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
    if (!profileMenu.hidden && !profilePicker.contains(event.target)) closeProfileMenu();
    if (state.inspectorPopup && !inspector.contains(event.target)) closeInspectorPopup();
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
    replaceSelection([]);
    state.config.profiles.push(profile);
    state.config.active_profile = profile.id;
    closeProfileCreator();
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
    (profileSelect.getClientRects().length ? profileSelect : profileAdd).focus();
  });
  profileCreateCancel.addEventListener("click", (event) => {
    event.preventDefault();
    closeProfileCreator(true);
  });
  $("profile-delete").addEventListener("click", () => {
    if (!state.config || state.config.profiles.length <= 1) return;
    const profile = activeProfile();
    if (!profile || !window.confirm(`Delete profile “${profile.name}”?`)) return;
    replaceSelection([]);
    state.config.profiles = state.config.profiles.filter((item) => item.id !== profile.id);
    state.config.active_profile = state.config.profiles[0].id;
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  });

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
    overlay.name = next;
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
    overlay.url = source.href;
    scheduleSave(true);
    requestRender("stageStructure", "inspector");
  }

  function commitInspectorOnEnter(event, commit) {
    if (event.key !== "Enter") return;
    event.preventDefault();
    event.stopPropagation();
    if (!event.repeat) commit();
  }

  nameField.addEventListener("change", commitTitle);
  nameField.addEventListener("keydown", (event) => commitInspectorOnEnter(event, commitTitle));
  urlField.addEventListener("change", commitUrl);
  urlField.addEventListener("keydown", (event) => commitInspectorOnEnter(event, commitUrl));

  for (const [name, input] of Object.entries(fields)) {
    input.addEventListener("focus", () => {
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
      const positionField = name === "left" || name === "top";
      if (positionField) {
        overlay[name] = value;
        const root = overlayRoots.get(overlay);
        if (root) applyOverlayStyle(root, overlay);
        updatePanelAvoidance();
        return;
      }
      applyInspectorFieldValue(name, overlay, value);
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
    input.addEventListener("blur", () => {
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
    });
  }

  function showSettingsError(message) {
    settingsError.textContent = message;
    settingsError.hidden = !message;
  }

  function renderSettingsDraft() {
    if (!state.settingsDraft) return;
    rightClickSettings.checked = state.settingsDraft.right_click_overlay_settings_menu;
    keepOverlaysInView.checked = state.settingsDraft.always_keep_overlays_in_view;
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
    closeInspectorPopup();
    const settings = state.config.settings;
    state.settingsDraft = {
      right_click_overlay_settings_menu: settings.right_click_overlay_settings_menu,
      always_keep_overlays_in_view: settings.always_keep_overlays_in_view,
      hotkeys: { ...settings.hotkeys }
    };
    state.recordingHotkey = "";
    showSettingsError("");
    renderSettingsDraft();
    settingsModal.hidden = false;
    rightClickSettings.focus();
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
    const pasted = state.copiedPlacements.map((overlay) => normalizeOverlay({ ...overlay }));
    for (const overlay of pasted) clampOverlay(overlay);
    profile.overlays.push(...pasted);
    replaceSelection(pasted);
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  openSettings.addEventListener("click", openSettingsModal);
  settingsCancel.addEventListener("click", () => closeSettingsModal());
  rightClickSettings.addEventListener("change", () => {
    if (state.settingsDraft) state.settingsDraft.right_click_overlay_settings_menu = rightClickSettings.checked;
  });
  keepOverlaysInView.addEventListener("change", () => {
    if (state.settingsDraft) state.settingsDraft.always_keep_overlays_in_view = keepOverlaysInView.checked;
  });
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
    const shortcuts = HOTKEY_KEYS.map((key) => state.settingsDraft.hotkeys[key]);
    if (new Set(shortcuts).size !== shortcuts.length) {
      showSettingsError("Each action must have a unique shortcut.");
      return;
    }
    state.config.settings = {
      right_click_overlay_settings_menu: state.settingsDraft.right_click_overlay_settings_menu,
      always_keep_overlays_in_view: state.settingsDraft.always_keep_overlays_in_view,
      hotkeys: normalizeHotkeys(state.settingsDraft.hotkeys)
    };
    closeSettingsModal();
    closeInspectorPopup();
    const profile = activeProfile();
    if (profile) for (const overlay of profile.overlays) clampOverlay(overlay);
    scheduleSave(true);
    requestRender("stageGeometry", "inspector");
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
    const counter = findCounter(overlay.counter_id);
    overlay.width = Math.max(10, finite(counter && counter.width, 500));
    overlay.height = Math.max(10, finite(counter && counter.height, 300));
    overlay.scale = 1;
    clampOverlay(overlay);
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
  $("open-available").addEventListener("click", () => ipc({ type: "open-available" }));
  $("done").addEventListener("click", () => setEditing(false));

  function isEditableTarget(target) {
    return target instanceof Element
      && (Boolean(target.closest("input, textarea, select")) || target.isContentEditable);
  }

  function menuOrModalOpen() {
    return !settingsModal.hidden || !urlModal.hidden || !profileMenu.hidden
      || !profileCreate.hidden || state.inspectorPopup;
  }

  document.addEventListener("wheel", (event) => {
    if (event.ctrlKey) event.preventDefault();
  }, { capture: true, passive: false });
  document.addEventListener("keydown", (event) => {
    if (!event.ctrlKey || event.altKey || event.metaKey) return;
    const zoomKey = ["+", "-", "=", "0"].includes(event.key)
      || ["Equal", "Minus", "NumpadAdd", "NumpadSubtract", "Digit0", "Numpad0"].includes(event.code);
    if (zoomKey) event.preventDefault();
  }, true);

  window.addEventListener("keydown", (event) => {
    const editable = isEditableTarget(event.target);
    if (state.editing && !event.repeat && !editable && !menuOrModalOpen()) {
      const shortcut = eventHotkey(event);
      const action = HOTKEY_KEYS.find((key) => state.config?.settings.hotkeys[key] === shortcut);
      if (action) {
        event.preventDefault();
        if (action === "copy_overlay") {
          copySelectedPlacements();
        } else if (action === "paste_overlay") {
          pasteCopiedPlacements();
        } else {
          const number = Number(action.slice("profile_".length));
          const profileIndex = number === 0 ? 9 : number - 1;
          const profile = state.config.profiles[profileIndex];
          if (profile) activateProfile(profile.id);
        }
        return;
      }
    }

    if (state.editing && event.ctrlKey && !event.altKey && !event.metaKey
        && !event.shiftKey && event.key.toLowerCase() === "r") {
      event.preventDefault();
      if (!editable && !event.repeat && !menuOrModalOpen()) refreshSelectedOverlay();
      return;
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
      const profile = activeProfile();
      if (!profile) return;
      let changed = false;
      for (const overlay of profile.overlays) {
        const before = `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}\n${overlay.rotation}`;
        clampOverlay(overlay);
        const after = `${overlay.left}\n${overlay.top}\n${overlay.width}\n${overlay.height}\n${overlay.scale}\n${overlay.rotation}`;
        if (before !== after) changed = true;
      }
      if (state.inspectorPopup) requestRender("inspector");
      if (!changed) return;
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
  });

  function receive(message) {
    try {
      const envelope = typeof message === "string" ? JSON.parse(message) : message;
      if (!envelope || typeof envelope !== "object") throw new Error("Invalid message from overlay host");
      if (envelope.type === "config") {
        replaceSelection([]);
        state.config = normalizeConfig(envelope.config !== undefined ? envelope.config : envelope.payload);
        const invalidSource = state.config.profiles
          .flatMap((item) => item.overlays)
          .find((overlay) => overlay.url && !customSource(overlay));
        if (invalidSource) setError(`Unsupported custom overlay source: ${invalidSource.url}`);
        else {
          state.error = null;
          status.textContent = "";
        }
        requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
      } else if (envelope.type === "saved") {
        if (state.latestSaveId && envelope.save_id === state.latestSaveId) status.textContent = "Saved";
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
