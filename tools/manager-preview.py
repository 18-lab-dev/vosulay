#!/usr/bin/env python3
"""Build a self-contained preview of the vosulay editor for any Chromium browser.

The page is assembled like src/ui.rs::manager_document, with a stub host bridge
that logs outbound messages and acknowledges saves (simulated; nothing is
persisted). No CEF, game, or running vosulay is needed.
"""
import argparse
import functools
import http.server
import json
import urllib.parse
import urllib.request
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


def javascript_string(source: str) -> str:
    # Mirrors src/ui.rs: a JSON string is a JavaScript string literal once the
    # characters that could end the inline <script> or a JS line are escaped.
    encoded = json.dumps(source, ensure_ascii=False)
    for raw, escaped in (("<", "\\u003c"), (">", "\\u003e"), ("&", "\\u0026"),
                         ("\u2028", "\\u2028"), ("\u2029", "\\u2029")):
        encoded = encoded.replace(raw, escaped)
    return encoded


def settings_origin(vosu_url: str) -> str:
    """Mirrors src/api.rs: vosu's settings page is on the dashboard origin it reports at
    /api/dashboard, or on vosu_url itself for tosu and vosu before the dashboard split."""
    try:
        with urllib.request.urlopen(urllib.parse.urljoin(vosu_url, "/api/dashboard"), timeout=10) as response:
            origin = json.load(response).get("origin")
    except (OSError, ValueError, AttributeError):
        return vosu_url
    parsed = urllib.parse.urlsplit(origin) if isinstance(origin, str) else None
    if (parsed and parsed.scheme in ("http", "https") and parsed.hostname and not parsed.username
            and parsed.path in ("", "/") and not parsed.query and not parsed.fragment):
        return origin
    return vosu_url


def build(config: dict, counters: list, vosu: dict | None = None) -> str:
    html = (ASSETS / "manager.html").read_text(encoding="utf-8")
    if html.count(MARKER) != 1:
        raise SystemExit(f"assets/manager.html must contain exactly one {MARKER}")
    css = javascript_string((ASSETS / "manager.css").read_text(encoding="utf-8"))
    javascript = javascript_string((ASSETS / "manager.js").read_text(encoding="utf-8"))
    host = """
window.__VOSULAY_SENT__ = [];
let __previewRevision = 0;
window.__VOSULAY_POST_MESSAGE__ = (payload) => {
  const message = typeof payload === "string" ? JSON.parse(payload) : payload;
  window.__VOSULAY_SENT__.push(message);
  console.log("[vosulay preview] host message", message);
  if (message.type === "save-config") {
    const revision = ++__previewRevision;
    setTimeout(() => window.__VOSULAY_RECEIVE__({ type: "saved", save_id: message.save_id, revision }), 5);
  }
  if (message.type === "refresh-counters" && window.__VOSULAY_PREVIEW_COUNTERS__) {
    setTimeout(() => window.__VOSULAY_RECEIVE__(window.__VOSULAY_PREVIEW_COUNTERS__), 5);
  }
};"""
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
    parser.add_argument("--vosu-url", metavar="URL",
                        help="vosu whose settings profiles the overlay settings dialog uses; "
                             "every counter then has settings (snapshot taken at build time)")
    args = parser.parse_args()

    config = json.loads(args.config.read_text(encoding="utf-8")) if args.config else DEMO_CONFIG
    ids = {overlay.get("counter_id") for profile in config.get("profiles", [])
           for overlay in profile.get("overlays", [])}
    ids = sorted(i for i in ids | set(DEMO_COUNTERS) if isinstance(i, str) and i)

    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "counter.html").write_text(DEMO_COUNTER, encoding="utf-8")
    counters = [{"id": i, "name": i, "author": "preview", "version": "1", "width": 300, "height": 100,
                 "has_settings": bool(args.vosu_url), "url": f"counter.html?{i}"} for i in ids]
    vosu = None
    if args.vosu_url:
        with urllib.request.urlopen(urllib.parse.urljoin(args.vosu_url, "/api/counters/profiles"), timeout=10) as response:
            registry = json.load(response)
        vosu = {"settings_url": urllib.parse.urljoin(settings_origin(args.vosu_url), "/settings"),
                "active": registry["active"], "profiles": registry["profiles"]}
    (args.output / "index.html").write_text(build(config, counters, vosu), encoding="utf-8")
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
