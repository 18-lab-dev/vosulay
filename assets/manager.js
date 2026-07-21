(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const stage = $("stage");
  const editor = $("editor");
  const inspector = $("inspector");
  const profileSelect = $("profile-select");
  const profileSelectLabel = $("profile-select-label");
  const profileMenu = $("profile-menu");
  const profilePicker = profileSelect.closest(".profile-picker");
  const profileAdd = $("profile-add");
  const profileCreate = $("profile-create");
  const profileName = $("profile-name");
  const profileCreateCancel = $("profile-create-cancel");
  const catalog = $("catalog");
  const status = $("status");
  const addUrl = $("add-url");
  const urlModal = $("url-modal");
  const urlForm = $("url-form");
  const urlInput = $("url-input");
  const urlCancel = $("url-cancel");
  const fields = {
    left: $("field-left"), top: $("field-top"), width: $("field-width"),
    height: $("field-height"), scale: $("field-scale"), z_index: $("field-z"),
    opacity: $("field-opacity"), rotation: $("field-rotation")
  };

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
    selected: null,
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
    return { version: 1, active_profile: ids.has(requested) ? requested : profiles[0].id, profiles };
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
    return profile && state.selected && profile.overlays.includes(state.selected) ? state.selected : null;
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
    const centerX = overlay.scale * (overlay.width * cosine - overlay.height * sine) / 2;
    const centerY = overlay.scale * (overlay.width * sine + overlay.height * cosine) / 2;
    overlay.left = clamp(finite(overlay.left, 0), -centerX, window.innerWidth - centerX);
    overlay.top = clamp(finite(overlay.top, 0), -centerY, window.innerHeight - centerY);
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
      name: counter ? counter.name : `${overlay.counter_id} (missing)`,
      url: counter ? counter.url : null,
      counter
    };
  }

  function overlayElement(overlay, details = overlayDetails(overlay)) {
    const root = makeElement("div", `overlay${details.available ? "" : " missing"}${state.selected === overlay ? " selected" : ""}`);
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
        root.classList.toggle("selected", state.selected === overlay);
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
    state.config.active_profile = id;
    state.selected = null;
    scheduleSave(true);
    closeProfileMenu(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  }

  function renderProfiles() {
    closeProfileMenu();
    profileMenu.replaceChildren();
    if (!state.config) {
      profileSelectLabel.textContent = "";
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

  function renderInspectorNumbers(overlay, names = Object.keys(fields)) {
    for (const name of names) {
      const value = inspectorFieldValue(name, overlay);
      if (fields[name].value !== value) fields[name].value = value;
    }
  }

  function renderInspector() {
    const overlay = selectedOverlay();
    inspector.hidden = !state.editing || !overlay;
    if (!overlay) return;
    const counter = findCounter(overlay.counter_id);
    if (overlay.url) {
      const source = customSource(overlay);
      $("selection-name").textContent = safeId(overlay.name, source ? source.hostname || "HTML overlay" : overlay.counter_id);
      $("selection-meta").textContent = source
        ? `${source.protocol === "vosulay-overlay:" ? "HTML file" : "URL"} · ${overlay.url}`
        : `Unsupported source · ${overlay.url}`;
    } else {
      $("selection-name").textContent = counter ? counter.name : overlay.counter_id;
      $("selection-meta").textContent = counter ? [counter.author, counter.version].filter(Boolean).join(" · ") : "Counter is not installed";
    }
    renderInspectorNumbers(overlay);
    $("settings").disabled = Boolean(overlay.url) || !counter || !counter.has_settings;
  }

  function render() {
    const regions = { ...dirty };
    for (const name of Object.keys(dirty)) dirty[name] = false;
    if (regions.chrome) {
      document.body.classList.toggle("editing", state.editing);
      editor.hidden = !state.editing;
    }
    if (regions.stageStructure || regions.stageGeometry) {
      renderStage(regions.stageStructure, regions.stageGeometry);
    }
    if (regions.profileList) renderProfiles();
    if (regions.catalog) renderCatalog();
    if (regions.inspector) renderInspector();
  }

  function selectOverlay(overlay) {
    if (state.selected === overlay) return;
    state.selected = overlay;
    requestRender("stageGeometry", "inspector");
  }

  function uniqueProfileId() {
    const taken = new Set(state.config.profiles.map((profile) => profile.id));
    let id;
    do id = `profile-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    while (taken.has(id));
    return id;
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
    state.selected = overlay;
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
    state.selected = overlay;
    scheduleSave();
    requestRender("stageStructure", "stageGeometry", "catalog", "inspector");
  }

  function removeSelected() {
    const profile = activeProfile();
    if (!profile || !state.selected) return;
    const index = profile.overlays.indexOf(state.selected);
    if (index < 0) return;
    profile.overlays.splice(index, 1);
    state.selected = null;
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
    const operation = state.pointer;
    if (operation) {
      if (operation.frame) {
        cancelAnimationFrame(operation.frame);
        operation.frame = 0;
        flushPointer(operation);
      }
      state.pointer = null;
      if (operation.root.hasPointerCapture(operation.id)) operation.root.releasePointerCapture(operation.id);
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
    urlInput.value = "https://";
    urlModal.hidden = false;
    urlInput.focus();
    urlInput.select();
  }

  function closeProfileCreator(restoreFocus = false) {
    if (profileCreate.hidden) return;
    profileCreate.hidden = true;
    if (restoreFocus) profileAdd.focus();
  }

  function openProfileCreator() {
    if (!state.config) return;
    profileName.value = `Profile ${state.config.profiles.length + 1}`;
    profileCreate.hidden = false;
    profileName.focus();
    profileName.select();
  }

  function setEditing(editing, notify = true) {
    const next = Boolean(editing);
    const changed = state.editing !== next;
    if (!next) {
      closeUrlModal();
      closeProfileMenu();
      closeProfileCreator();
    }
    if (!next) flushEditingChanges();
    if (changed) {
      state.editing = next;
      if (!next) state.selected = null;
      requestRender("chrome", "stageGeometry", "inspector");
    }
    if (notify && changed) ipc({ type: "set-editing", editing: next });
    else if (!notify && !next) ipc({ type: "editing-applied", editing: false });
  }

  function startPointer(event, root, edge) {
    if (!state.editing || event.button !== 0) return;
    const overlay = selectedOverlay();
    if (!overlay || root._vosulayOverlay !== overlay) return;
    event.preventDefault();
    root.setPointerCapture(event.pointerId);
    const angle = overlay.rotation * Math.PI / 180;
    state.pointer = {
      id: event.pointerId,
      root,
      overlay,
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
    const overlay = operation.overlay;
    const screenX = operation.latestX - operation.x;
    const screenY = operation.latestY - operation.y;
    let affected;
    if (!operation.edge) {
      overlay.left = operation.left + screenX;
      overlay.top = operation.top + screenY;
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
    }
    clampOverlay(overlay);
    applyOverlayStyle(operation.root, overlay);
    if (state.editing && state.selected === overlay) renderInspectorNumbers(overlay, affected);
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
    scheduleSave(true);
    requestRender("stageGeometry", "inspector");
  }

  stage.addEventListener("pointerdown", (event) => {
    if (!state.editing) return;
    const root = event.target.closest(".overlay");
    if (!root) {
      selectOverlay(null);
      return;
    }
    selectOverlay(root._vosulayOverlay);
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
    selectOverlay(root._vosulayOverlay);
  });
  stage.addEventListener("wheel", (event) => {
    if (!state.editing) return;
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
  }, true);
  profilePicker.addEventListener("focusout", (event) => {
    if (!profileMenu.hidden && !profilePicker.contains(event.relatedTarget)) closeProfileMenu();
  });
  profileAdd.addEventListener("click", openProfileCreator);
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
    state.config.profiles.push(profile);
    state.config.active_profile = profile.id;
    state.selected = null;
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
    state.config.profiles = state.config.profiles.filter((item) => item.id !== profile.id);
    state.config.active_profile = state.config.profiles[0].id;
    state.selected = null;
    scheduleSave(true);
    requestRender("stageStructure", "stageGeometry", "profileList", "catalog", "inspector", "chrome");
  });

  for (const [name, input] of Object.entries(fields)) {
    input.addEventListener("input", () => {
      const overlay = selectedOverlay();
      if (!overlay || input.value === "") return;
      const value = Number(input.value);
      if (!Number.isFinite(value)) return;
      if (name === "width" || name === "height") overlay[name] = Math.max(10, value);
      else if (name === "scale") overlay.scale = Math.max(0.1, value);
      else if (name === "z_index") overlay.z_index = Math.round(clamp(value, 0, 999));
      else if (name === "opacity") overlay.opacity = clamp(value / 100, 0, 1);
      else if (name === "rotation") overlay.rotation = clamp(value, -180, 180);
      else overlay[name] = value;
      clampOverlay(overlay);
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
    input.addEventListener("change", () => {
      const overlay = selectedOverlay();
      if (!overlay) return;
      clampOverlay(overlay);
      scheduleSave();
      requestRender("stageGeometry", "inspector");
    });
  }

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
    const source = parseSource(urlInput.value, ["http:", "https:"]);
    if (!source) {
      setError("Overlay URL must be a valid http or https URL");
      urlInput.focus();
      urlInput.select();
      return;
    }
    addCustomOverlay(source, deriveUrlName(source));
    closeUrlModal();
  });
  $("add-html-file").addEventListener("click", () => ipc({ type: "choose-overlay-file" }));

  $("refresh").addEventListener("click", () => {
    status.textContent = "Refreshing…";
    ipc({ type: "refresh-counters" });
  });
  $("open-available").addEventListener("click", () => ipc({ type: "open-available" }));
  $("quit").addEventListener("click", () => {
    if (window.confirm("Quit vosulay?")) ipc({ type: "quit" });
  });
  $("done").addEventListener("click", () => setEditing(false));

  window.addEventListener("keydown", (event) => {
    if (state.editing && event.ctrlKey && !event.altKey && !event.metaKey
        && !event.shiftKey && event.key.toLowerCase() === "r") {
      event.preventDefault();
      if (!event.repeat && urlModal.hidden) refreshSelectedOverlay();
      return;
    }

    if (event.key === "Delete") {
      const target = event.target;
      const editable = target instanceof Element
        && (target.closest("input, textarea, select") || target.isContentEditable);
      if (event.repeat || urlModal.hidden === false || editable || !state.editing || !selectedOverlay()) return;
      removeSelected();
      event.preventDefault();
      return;
    }

    if (event.key !== "Escape") return;
    if (event.repeat) {
      if (!urlModal.hidden || !profileMenu.hidden || !profileCreate.hidden || state.editing) event.preventDefault();
      return;
    }
    if (!urlModal.hidden) {
      event.preventDefault();
      closeUrlModal();
    } else if (!profileMenu.hidden) {
      event.preventDefault();
      closeProfileMenu(true);
    } else if (!profileCreate.hidden) {
      event.preventDefault();
      closeProfileCreator(true);
    } else if (state.editing) {
      event.preventDefault();
      setEditing(false);
    }
  });
  window.addEventListener("resize", () => {
    if (state.resizeFrame) return;
    state.resizeFrame = requestAnimationFrame(() => {
      state.resizeFrame = 0;
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
        state.config = normalizeConfig(envelope.config !== undefined ? envelope.config : envelope.payload);
        const profile = activeProfile();
        if (!profile || !profile.overlays.includes(state.selected)) state.selected = null;
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
