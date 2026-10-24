#!/usr/bin/env python3
"""Build a self-contained preview of the vosulay editor for any Chromium browser.

The page is assembled like src/ui.rs::manager_document, with a stub host bridge
that logs outbound messages and acknowledges saves (simulated; nothing is
persisted). No CEF, game, or running vosulay is needed.
"""
import argparse
import base64
import functools
import http.server
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
MARKER = "__VOSULAY_BOOTSTRAP__"

DEMO_COUNTER = """<!doctype html><meta charset="utf-8">
<body style="margin:0;font:600 18px system-ui;color:#fff;display:grid;place-items:center;
height:100vh;background:linear-gradient(135deg,rgba(139,124,255,.55),rgba(122,225,248,.45))">
<span id="label"></span>
<script>label.textContent = decodeURIComponent(location.search.slice(1)) || "counter";</script>
"""

DEMO_CONFIG = {
    "version": 1,
    "active_profile": "default",
    "profiles": [
        {
            "id": "default",
            "name": "Default",
            "overlays": [
                {"counter_id": "hit-error-bar", "left": 460, "top": 520, "width": 360, "height": 60,
                 "scale": 1.0, "opacity": 1.0, "rotation": 0.0, "enabled": True, "z_index": 0},
                {"counter_id": "accuracy", "left": 1000, "top": 40, "width": 220, "height": 80,
                 "scale": 1.0, "opacity": 1.0, "rotation": 0.0, "enabled": True, "z_index": 1},
            ],
        },
        {"id": "second", "name": "Second", "overlays": []},
    ],
    "settings": {},
}

DEMO_COUNTERS = ["hit-error-bar", "accuracy", "pp-counter", "key-overlay"]

DEMO_SETTINGS = [
    {"type": "header", "uniqueID": "appearance", "title": "Appearance"},
    {"type": "text", "uniqueID": "label", "title": "Counter label", "value": "Demo counter",
     "description": "Text displayed above the counter."},
    {"type": "color", "uniqueID": "accent", "title": "Accent colour", "value": "#8b7cff"},
    {"type": "number", "uniqueID": "precision", "title": "Decimal places", "value": 2},
    {"type": "checkbox", "uniqueID": "show_label", "title": "Show label", "value": True},
    {"type": "options", "uniqueID": "alignment", "title": "Alignment", "value": "Center",
     "options": ["Left", "Center", "Right"]},
    {"type": "header", "uniqueID": "behaviour", "title": "Behaviour and commands"},
    {"type": "button", "uniqueID": "help", "title": "Overlay documentation",
     "value": "https://github.com/18-lab-dev/vosulay"},
    {"type": "textarea", "uniqueID": "notes", "title": "Notes", "value": "First line\nSecond line"},
    {"type": "password", "uniqueID": "token", "title": "Demo token", "value": "preview-only"},
    {"type": "commands", "uniqueID": "commands", "title": "Chat commands", "uniqueCheck": "command",
     "description": "Command names must be nonempty and unique.",
     "options": [
         {"name": "command", "title": "Command", "type": "text", "value": "!new"},
         {"name": "response", "title": "Response", "type": "text", "value": "Hello"},
         {"name": "cooldown", "title": "Cooldown", "type": "number", "value": 5},
         {"name": "enabled", "title": "Enabled", "type": "checkbox", "value": True},
         {"name": "audience", "title": "Audience", "type": "options", "value": "Everyone",
          "values": ["Everyone", "Subscribers", "Moderators"]},
     ],
     "value": [
         {"command": "!pp", "response": "Performance", "cooldown": 5, "enabled": True, "audience": "Everyone"},
         {"command": "!accuracy", "response": "Accuracy", "cooldown": 10, "enabled": False, "audience": "Moderators"},
     ]},
]


def javascript_string(source: str) -> str:
    # Mirrors src/ui.rs: a JSON string is a JavaScript string literal once the
    # characters that could end the inline <script> or a JS line are escaped.
    encoded = json.dumps(source, ensure_ascii=False)
    for raw, escaped in (("<", "\\u003c"), (">", "\\u003e"), ("&", "\\u0026"),
                         ("\u2028", "\\u2028"), ("\u2029", "\\u2029")):
        encoded = encoded.replace(raw, escaped)
    return encoded


def build(config: dict, counters: list, vosu_demo: bool = False) -> str:
    html = (ASSETS / "manager.html").read_text(encoding="utf-8")
    if html.count(MARKER) != 1:
        raise SystemExit(f"assets/manager.html must contain exactly one {MARKER}")
    styles = (ASSETS / "manager.css").read_text(encoding="utf-8")
    for font in (ASSETS / "fonts").glob("*.woff2"):
        styles = styles.replace("vosulay-font:" + font.name,
                                "data:font/woff2;base64," + base64.b64encode(font.read_bytes()).decode("ascii"))
    css = javascript_string(styles)
    javascript = javascript_string((ASSETS / "manager.js").read_text(encoding="utf-8"))
    vosu = {"active": "default", "profiles": ["default", "stream"], "writable": True, "reason": None} if vosu_demo else None
    host = """
const __previewClone = value => JSON.parse(JSON.stringify(value));
const __previewProfiles = new Map([["default", {}], ["stream", {}]]);
window.__VOSULAY_FAIL_NEXT__ = false;
function __previewVosuRequest(message) {
  const catalog = window.__VOSULAY_PREVIEW_COUNTERS__;
  const registry = catalog.vosu;
  const reply = value => setTimeout(() => window.__VOSULAY_RECEIVE__(
    __previewClone({ ...value, request_id: message.request_id ?? null })), 5);
  let readOnly = null;
  try {
    if (!registry) throw new Error("Start the preview with --vosu-demo to enable native settings.");
    const writing = message.type === "vosu-settings-save" || message.type === "vosu-profile-action";
    const failure = window.__VOSULAY_FAIL_NEXT__;
    const readOnlyFailure = failure && typeof failure === "object" && typeof failure.read_only === "string";
    if (failure && (!readOnlyFailure || writing)) {
      window.__VOSULAY_FAIL_NEXT__ = false;
      if (readOnlyFailure) {
        registry.writable = false;
        registry.reason = failure.read_only;
        readOnly = failure.read_only;
      }
      throw new Error(readOnly ?? (typeof failure === "string" ? failure : "Simulated vosu request failure"));
    }
    if (writing && !registry.writable) {
      readOnly = registry.reason ?? "Vosu settings are read-only.";
      throw new Error(readOnly);
    }
    if (message.type === "vosu-profile-action") {
      const { action, name, target, copy_from } = message;
      const validName = value => typeof value === "string" && /^[A-Za-z0-9_-]{1,48}$/.test(value);
      if (!validName(name)) throw new Error("Invalid profile name.");
      if (action === "create") {
        if (__previewProfiles.has(name)) throw new Error("Profile already exists.");
        if (copy_from != null && !__previewProfiles.has(copy_from)) throw new Error("Source profile is missing.");
        __previewProfiles.set(name, copy_from == null ? {} : __previewClone(__previewProfiles.get(copy_from)));
      } else {
        if (!__previewProfiles.has(name)) throw new Error("Profile is missing.");
        if (action === "rename") {
          if (name === "default") throw new Error("The default profile cannot be renamed.");
          if (!validName(target)) throw new Error("Invalid new profile name.");
          if (__previewProfiles.has(target)) throw new Error("Profile already exists.");
          __previewProfiles.set(target, __previewProfiles.get(name));
          __previewProfiles.delete(name);
          if (registry.active === name) registry.active = target;
        } else if (action === "delete") {
          if (name === "default") throw new Error("The default profile cannot be deleted.");
          __previewProfiles.delete(name);
          if (registry.active === name) registry.active = "default";
        } else if (action === "activate") {
          registry.active = name;
        } else throw new Error("Unsupported profile action.");
      }
      registry.profiles = Array.from(__previewProfiles.keys());
      reply({ type: "vosu-profiles", registry });
      return;
    }
    const profile = message.profile ?? null;
    const selected = profile ?? registry.active;
    if (!__previewProfiles.has(selected)) throw new Error("Settings profile is missing.");
    if (!catalog.counters.some(counter => counter.id === message.counter_id)) throw new Error("Counter is missing.");
    const stored = __previewProfiles.get(selected);
    if (message.type === "vosu-settings-save") {
      if (!Array.isArray(message.values) || message.values.some(entry =>
        !entry || typeof entry.uniqueID !== "string" || !Object.hasOwn(entry, "value"))) {
        throw new Error("Each value needs a uniqueID and a value.");
      }
      const values = stored[message.counter_id] ??= {};
      message.values.forEach(entry => { values[entry.uniqueID] = __previewClone(entry.value); });
      reply({ type: "vosu-settings-saved", counter_id: message.counter_id, profile });
    } else {
      reply({ type: "vosu-settings", counter_id: message.counter_id, profile,
        settings: __previewSchema, values: stored[message.counter_id] ?? {}, registry });
    }
  } catch (error) {
    reply({ type: "vosu-settings-error", message: error.message,
      ...(readOnly == null ? {} : { read_only: readOnly }) });
  }
}
window.__VOSULAY_SENT__ = [];
let __previewRevision = 0;
window.__VOSULAY_POST_MESSAGE__ = (payload) => {
  const message = typeof payload === "string" ? JSON.parse(payload) : payload;
  window.__VOSULAY_SENT__.push(message);
  console.log("[vosulay preview] host message", message);
  if (["vosu-settings-load", "vosu-settings-save", "vosu-profile-action"].includes(message.type)) {
    __previewVosuRequest(message);
    return;
  }
  if (message.type === "save-config") {
    const revision = ++__previewRevision;
    setTimeout(() => window.__VOSULAY_RECEIVE__({ type: "saved", save_id: message.save_id, revision }), 5);
  }
  if (message.type === "refresh-counters" && window.__VOSULAY_PREVIEW_COUNTERS__) {
    setTimeout(() => window.__VOSULAY_RECEIVE__(window.__VOSULAY_PREVIEW_COUNTERS__), 5);
  }
};"""
    host = f"const __previewSchema=JSON.parse({javascript_string(json.dumps(DEMO_SETTINGS))});" + host
    receive = (
        f"window.__VOSULAY_RECEIVE__({{type:'config',revision:0,config:JSON.parse({javascript_string(json.dumps(config))})}});"
        "window.__VOSULAY_RECEIVE__({type:'editing',editing:true});"
        f"window.__VOSULAY_PREVIEW_COUNTERS__={{type:'counters',counters:JSON.parse({javascript_string(json.dumps(counters))}),"
        f"vosu:JSON.parse({javascript_string(json.dumps(vosu))})}};"
        "window.__VOSULAY_RECEIVE__(window.__VOSULAY_PREVIEW_COUNTERS__);"
    )
    bootstrap = (
        f"(()=>{{{host}const style=document.createElement('style');style.textContent={css};"
        f"document.head.appendChild(style);(0,eval)({javascript});{receive}}})();"
    )
    return html.replace(MARKER, bootstrap, 1)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--output", type=Path, default=ROOT / "target" / "manager-preview",
                        help="output directory (default target/manager-preview)")
    parser.add_argument("--config", type=Path, help="vosulay config.json to load instead of the demo profiles")
    parser.add_argument("--serve", type=int, metavar="PORT",
                        help="also serve the preview on http://127.0.0.1:PORT/ until interrupted")
    parser.add_argument("--vosu-demo", action="store_true",
                        help="enable an in-memory vosu settings/profile demo; no backend or persisted writes")
    args = parser.parse_args()

    config = json.loads(args.config.read_text(encoding="utf-8")) if args.config else DEMO_CONFIG
    ids = {overlay.get("counter_id") for profile in config.get("profiles", [])
           for overlay in profile.get("overlays", [])}
    ids = sorted(i for i in ids | set(DEMO_COUNTERS) if isinstance(i, str) and i)

    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "counter.html").write_text(DEMO_COUNTER, encoding="utf-8")
    counters = [{"id": i, "name": i, "author": "preview", "version": "1", "width": 300, "height": 100,
                 "has_settings": args.vosu_demo, "url": f"counter.html?{i}"} for i in ids]
    (args.output / "index.html").write_text(build(config, counters, args.vosu_demo), encoding="utf-8")
    index = (args.output / "index.html").resolve()
    print(f"Editor preview: {index.as_uri()}")

    if args.serve is not None:
        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(args.output))
        with http.server.ThreadingHTTPServer(("127.0.0.1", args.serve), handler) as server:
            print(f"Serving on http://127.0.0.1:{server.server_address[1]}/ (Ctrl+C to stop)")
            try:
                server.serve_forever()
            except KeyboardInterrupt:
                pass


if __name__ == "__main__":
    main()
