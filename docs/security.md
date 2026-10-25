# Security, installation, and system behavior

This disclosure describes the repository's implementation, not a certification of
third-party binaries or an executed installation/network audit. Links identify the
code behind each behavior; unresolved third-party behavior is called out below.

## What vosulay does

Vosulay renders web counters and an editor off-screen using CEF, sends the rendered
frames to an in-game compositor, and brokers data from a configured vosu/tosu
backend. It starts platform integration and remote listeners when the host starts;
CEF itself initializes lazily when a game view opens. Sources:
[`src/runtime/host.rs`](../src/runtime/host.rs),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs).

## Installation, storage, and removal

### Windows

- The Inno Setup installer defaults to `%LOCALAPPDATA%\Programs\Vosulay` and
  requests the lowest privilege level (per-user installation). It copies the
  complete staged runtime: the CEF bootstrap executable, vosulay client DLL, CEF
  libraries/resources/locales, and packaged overlay DLLs. Sources:
  [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss),
  [`crates/cef-tools/src/package.rs`](../crates/cef-tools/src/package.rs).
- It creates a per-user Start Menu shortcut. Desktop and Windows Startup shortcuts
  are separate, initially unchecked installer tasks. The final installer page can
  launch vosulay; silent installation does not perform that launch. The application
  also creates or updates `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vosulay.lnk`
  to point to the running executable, including when using the ZIP distribution.
  Sources: [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss),
  [`src/runtime/desktop_entry.rs`](../src/runtime/desktop_entry.rs).
- The installer provides normal Inno uninstall tracking of installed files and
  shortcuts, but deliberately retains user settings/profiles. No custom
  `UninstallDelete` cleanup is specified. Installation, upgrade, and uninstall
  behavior has not been exercised for this disclosure. Source:
  [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss).
- Inno Setup's standard per-user uninstall registration adds an entry under
  `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall` for
  the installer's AppId and writes its uninstaller into the install directory.
  The script has no custom `[Registry]` entries, service, scheduled task, or
  firewall-rule installation. Autostart uses only the optional Startup shortcut
  above, not a registry Run key. Source:
  [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss).

### Linux

- DEB/RPM/Arch binary packages install the runtime under `/usr/lib/vosulay`, a
  `/usr/bin/vosulay` symlink, the system desktop entry
  `/usr/share/applications/com.vosu.vosulay.desktop`, and the SVG icon under
  `/usr/share/icons/hicolor/scalable/apps`. The runtime includes CEF, compositor
  and lookup shared libraries, and `VkLayer_vosulay_overlay.json`. The tarball
  contains the staged runtime and can be extracted to a chosen directory; it does
  not itself run a system installer. Sources:
  [`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml),
  [`packaging/linux/build.sh`](../packaging/linux/build.sh),
  [`crates/cef-tools/src/package.rs`](../crates/cef-tools/src/package.rs).
- **The Linux packages install `chrome-sandbox` as root:root, mode `4755`
  (setuid root).** This is Chromium's sandbox helper for systems restricting
  unprivileged user namespaces, not elevation of the overlay host. The Arch source
  package also sets mode `4755`; extracting a tarball alone does not perform this
  privileged ownership/mode setup. Sources:
  [`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml),
  [`packaging/arch/PKGBUILD`](../packaging/arch/PKGBUILD),
  [`packaging/linux/build.sh`](../packaging/linux/build.sh).
- At startup vosulay self-registers a user launcher and icon under
  `$XDG_DATA_HOME` (normally `~/.local/share`) unless an identically named system
  desktop entry exists in `$XDG_DATA_DIRS`. It can also write a user Vulkan layer
  manifest at `$XDG_DATA_HOME/vosulay/vulkan/VkLayer_vosulay_overlay.json` during
  osu! launcher integration. Sources:
  [`src/runtime/desktop_entry.rs`](../src/runtime/desktop_entry.rs),
  [`src/runtime/launchers.rs`](../src/runtime/launchers.rs).
- Package metadata specifies installed system files, but no removal hook to undo
  the application's user launcher files, game-shortcut edits, configuration, or
  browser storage. Removing the package is therefore not a promise to restore
  user game shortcuts; inspect/remove the vosulay environment additions or user
  overrides yourself. Sources: [`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml),
  [`src/runtime/launchers.rs`](../src/runtime/launchers.rs).

### Configuration and caches

The default configuration is `~/.config/vosulay/config.json` on Linux (respecting
XDG configuration conventions) or `%APPDATA%\vosulay\config.json` on Windows.
`--config` overrides it; an executable-adjacent `portable` marker or existing
`data/config.json` selects portable storage instead. Profiles, URLs, settings,
and local-file capability roots are stored in that JSON. CEF's persistent browser
cache and `cef.log` are in `cef-cache` **beside the selected configuration**, not
necessarily in the OS cache directory. On Linux, CEF runs with
`--password-store=basic`: Chromium encrypts that cache's cookies with its fixed
local key instead of a key from the desktop keyring, so they are protected only
by the directory's permissions. vosulay keeps no passwords there, and its broker
secret and vosu token never reach cookies or web storage. A locked or prompting
keyring otherwise held every HTTP request, leaving all counters blank. Cookies
that an earlier release encrypted with a keyring key may no longer be readable,
so counters can lose them once. Sources: [`src/config.rs`](../src/config.rs),
[`src/runtime.rs`](../src/runtime.rs),
[`src/runtime/host.rs`](../src/runtime/host.rs),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs).

Build/acquisition tooling separately retains downloaded CEF assets beneath
`$XDG_CACHE_HOME/vosulay` or `$HOME/.cache/vosulay` (falling back to
`%USERPROFILE%/.cache/vosulay`); this is not the installed application's browser
cache. Linux's single-instance lock uses `$XDG_RUNTIME_DIR/vosulay/instance.lock`,
falling back to the user's cache directory. Sources:
[`crates/cef-tools/src/lib.rs`](../crates/cef-tools/src/lib.rs),
[`src/runtime/instance.rs`](../src/runtime/instance.rs).

### vosu-managed installation

Tool mode is explicitly selected by `VOSU_TOOL_PROTOCOL=1`. vosu supervises the
process with the user's OS rights; catalog trust is not an OS sandbox. The
backend is forced to `VOSU_URL`, persistent state stays in `VOSU_TOOL_DATA_DIR`
and browser cache/logs in `VOSU_TOOL_CACHE_DIR`. Initial import copies (never
moves or rewrites) the standalone config only when tool data is empty.
Tool startup suppresses launcher registration and Linux game-shortcut rewriting;
it does not remove existing standalone registrations.
Standalone launches discover enabled-tool ownership only through vosu's validated
local broker file and authenticated native CLI API, with bounded HTTP requests.
The credential and one-use editor launch code are never logged. Managed startup
may ask a same-user standalone host to flush and yield over the existing local
control channel; supervised peers refuse. It never force-kills a peer. There is
no managed tray Quit action: vosu closes stdin to stop the tool.

The protocol HTTP listener binds 127.0.0.1 on an assigned port and requires the
exact per-start `X-Vosu-Tool-Token` on every request, including health checks.
The separate counter broker is still loopback, with its existing authority
checks; no configured standalone remote/LAN listener is started in tool mode.
The standalone manage origin is disabled. Dashboard UI sessions authorize the
write-only `/v1/manage` proxy; view-only `/v1/view` needs no UI session.
Sessions remain only in the page's fetch closure and never enter storage,
cookies, counter frames or logs. Tool-manager frames use an opaque-origin
sandbox, so even counters on vosu's shared origin cannot reach the bridge.
The editor worker retains its message/capability checks. Existing native game
attachment, input and CEF sandbox disclosures still apply to this separate tool.

#### Browser preview capability

`Origin: null` is not provenance: any website can create an opaque sandboxed frame.
Tool previews therefore use an independent per-run 256-bit capability hostname
(two DNS-safe random hex labels under `.preview.localhost`) on the loopback broker.
Its exact authority/port is compared without secret-dependent early exit and only
loopback peers are admitted. Only capability-bearing counter resources receive
opaque-origin CORS without credentials and cross-origin resource embedding.
Plain/guessed hosts receive no new access. `/overlay`, `__vosulay/*`, manager and
tool/config routes stay closed on the preview authority, even if the caller knows
the capability. A preview socket never receives the native broker credential.

The counter can read its own location and exfiltrate this capability. A recipient
then gains only the public counter resources/data sockets the counter could load,
and local files only if it also possesses that file's independently unguessable,
configured root capability. Canonical-path confinement remains enforced; no root
enumeration, profiles/configuration, manager session or native write authority is
granted. This is a read-resource capability, not an editor authentication token.

Only authenticated manager responses disclose it; public view/state responses do
not. It is generated in memory, regenerated on restart, never persisted or logged
by the host, and suppressed in Referer by frame/response policy. Remote tool pages
do not attempt these viewer-local addresses: they show a local-only preview reason
without an unscoped fallback. Existing native CEF/standalone policies are unchanged.


### How to remove vosulay completely

1. Stop the managed tool in **vosu → Tools**, or choose standalone
   **tray → Exit vosulay** (Hide is not Exit), and close games that loaded
   the overlay before replacing/removing libraries. Stopping closes the host and its
   listeners; restarting the game without vosulay integration is the way to
   ensure no previously injected code remains loaded.
2. **Windows installer:** use **Settings → Apps → Installed apps / Apps & features
   → Vosulay → Uninstall**, or the installed Inno uninstaller. It tracks installed
   files, shortcuts, and uninstall registration. **Windows ZIP:** delete the
   extracted directory and remove the self-created
   `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vosulay.lnk`.
   Remove any optional Desktop/Startup shortcuts left from an earlier install
   to prevent attempts to start a removed executable.
3. **Linux package:** use `sudo apt remove vosulay`, `sudo dnf remove vosulay`,
   or `sudo pacman -R vosulay`. **Linux tarball:** remove the extracted directory.
   For Cargo installs use `cargo uninstall vosulay`.
4. **Linux user integration remains:** inspect recognized osu! desktop entries
   under `$XDG_DATA_HOME/applications` (normally `~/.local/share/applications`).
   Remove only vosulay's preload paths and Vulkan environment additions,
   preserving foreign preloads, arguments, and other layers. Delete a generated
   system-entry override only if no other customization needs preserving.
   Remove the self-created `com.vosu.vosulay.desktop` entry, the
   `icons/hicolor/scalable/apps/com.vosu.vosulay.svg` icon and
   `vosulay/vulkan/VkLayer_vosulay_overlay.json` under `$XDG_DATA_HOME` if present.
5. Settings/profiles and CEF cache/logs are deliberately retained. For full data
   removal, delete only the selected configuration directory and its `cef-cache`
   after saving any profiles you want to keep (default paths above; portable
   storage is `data` beside the executable). Optional build-tool assets under
   `$XDG_CACHE_HOME/vosulay` / `$HOME/.cache/vosulay` can be removed separately
   when no source-built install still depends on them. Do not delete unrelated
   overlay directories or backend data.

These instructions describe the implementation and package-manager removal
paths; they are not a claim of native uninstall testing.

## Changes to games and other software

**Linux startup automatically rewrites recognized osu! desktop launch commands.**
It scans user and system application entries, modifies user entries in place, and
creates user overrides for system entries rather than changing system files. It
configures `LD_PRELOAD` (lookup helper, foreign preloads, then compositor) and
Vulkan layer environment variables, including an absolute-path user manifest.
Hidden entries remain hidden; unsupported command forms and D-Bus activation are
not rewritten. There is no interactive permission step in this startup path.
Sources: [`src/runtime/host.rs`](../src/runtime/host.rs),
[`src/runtime/launchers.rs`](../src/runtime/launchers.rs).

**Vosulay loads code into game processes.** Windows selects supported foreground
game processes and calls `asdf_overlay_client::inject` using packaged
`asdf_overlay-x64.dll`/`asdf_overlay-x86.dll`. Its integration exchanges input and
D3D11 frames with that injected overlay. These DLLs are third-party prebuilt
assets: this repository's callsites do not independently establish all behavior
inside them. Sources:
[`crates/platform-windows/src/platform.rs`](../crates/platform-windows/src/platform.rs),
[`crates/platform-windows/src/foreground.rs`](../crates/platform-windows/src/foreground.rs),
[`tools/cef/pins.json`](../tools/cef/pins.json).

Linux supports loading the compositor at game startup through `LD_PRELOAD` or
its Vulkan layer. It also scans `/proc` and automatically attempts live attachment
to supported processes not already connected, using `ptrace` to load the library
and initialize its endpoint. `--pid` and `--process-name` can extend target
selection. Processes that already map a vosulay compositor or lookup helper are
skipped; they connect by themselves. Live attachment requires a native x86_64
ELF process owned by the host's effective UID and not hosted by Wine (Wine games
are refused before any `ptrace` call); it temporarily stops the target and
restores registers and detaches afterward. Sources:
[`crates/platform-linux/src/lib.rs`](../crates/platform-linux/src/lib.rs),
[`crates/platform-linux/src/injector.rs`](../crates/platform-linux/src/injector.rs),
[`src/runtime.rs`](../src/runtime.rs).

The Linux injected code hooks graphics presentation (GLX/EGL/SDL and Vulkan),
composites vosulay frames onto game output, observes focus and input, and can
redirect keyboard/pointer input to the editor rather than the game. The lookup
helper redirects relevant explicit symbol lookups to the compositor. Sources:
[`crates/overlay-linux/src/hooks.rs`](../crates/overlay-linux/src/hooks.rs),
[`crates/overlay-linux/src/vulkan/`](../crates/overlay-linux/src/vulkan/),
[`crates/overlay-linux/src/sdl.rs`](../crates/overlay-linux/src/sdl.rs),
[`crates/overlay-linux/src/x11input.rs`](../crates/overlay-linux/src/x11input.rs),
[`crates/overlay-lookup/src/lib.rs`](../crates/overlay-lookup/src/lib.rs).

The injection and hooks are for rendering user-selected counters and routing
editor gestures in the selected game, not for bypassing attachment restrictions.
Starting vosulay enables automatic discovery/attachment; there is no per-target
consent dialog. Default targets are supported osu! processes; on Linux, explicit
`--pid` or `--process-name` selections widen that scope. The user controls counters,
edit mode and interaction through the UI and can stop the host through **Exit
vosulay**. Hiding the overlay does **not** stop attachment or listeners; quitting
does not undo Linux launcher edits. Close/relaunch the game after quitting to
clear already loaded overlay code. Do not use attachment against software whose
rules prohibit it, or weaken OS security policy to make attachment succeed.

Foundation eligibility for process injection, graphics/input/symbol hooks and
the unrestricted SharedArrayBuffer compatibility opt-in must be confirmed with
SignPath before claiming approval. In particular, the opt-in below weakens a
browser security protection; explaining its purpose is not evidence that it
meets the Foundation's prohibition on circumventing security measures.

## Privileges and input access

The Windows installer does not request elevation. If the user deliberately starts
vosulay elevated, its CEF command-line policy adds `do-not-de-elevate` to retain
the chosen host privileges; it does not disable CEF subprocess sandboxing.
Windows initialization requires the packaged sandbox bootstrap. Sources:
[`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs).

Linux live attachment does not change kernel ptrace policy or request privilege
escalation: failed ptrace operations are reported, suggesting preload/layer
startup instead. Same-UID checking applies even if the host is started as root;
root is not a code-level bypass for attaching to another UID. Kernel Yama/LSM,
namespace, and process restrictions can still prevent attachment. Sources:
[`crates/platform-linux/src/injector.rs`](../crates/platform-linux/src/injector.rs),
[`crates/platform-linux/src/lib.rs`](../crates/platform-linux/src/lib.rs).
System package installation and setting the sandbox helper's ownership/mode are
privileged administrator/package-manager operations, separate from normal host
execution. Source: [`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml).

Editing captures game keyboard/pointer input for the embedded editor; capture is
gated on the attached game being focused, with focus-loss and heartbeat handling.
Windows registers configurable OS-global hotkeys with `RegisterHotKey`; Linux
recognizes shortcuts in the connected game's input stream, not a desktop-wide
key grab. The default edit shortcut is `Ctrl+Shift+Space`. Sources:
[`src/runtime/host.rs`](../src/runtime/host.rs), [`src/config.rs`](../src/config.rs),
[`crates/platform-windows/src/tray.rs`](../crates/platform-windows/src/tray.rs),
[`crates/platform-windows/src/platform.rs`](../crates/platform-windows/src/platform.rs),
[`crates/platform-linux/src/lib.rs`](../crates/platform-linux/src/lib.rs),
[`crates/overlay-linux/src/x11input.rs`](../crates/overlay-linux/src/x11input.rs).

On Linux the host connects to the system clipboard using Wayland wlr data-control
(which permits access without keyboard focus), falling back to an X11 clipboard
client. Editor bridge requests can read or replace clipboard text, limited to
1 MiB with timeouts; this is not a promise that the compositor itself prompts
for clipboard access. The native bridge is restricted to the exact trusted
manager main frame, not arbitrary counter frames. Sources:
[`src/runtime/clipboard.rs`](../src/runtime/clipboard.rs),
[`src/runtime/clipboard/x11.rs`](../src/runtime/clipboard/x11.rs),
[`src/runtime/editor.rs`](../src/runtime/editor.rs),
[`crates/browser-cef/src/bridge.rs`](../crates/browser-cef/src/bridge.rs).

## Network exposure and web content

The configured backend defaults to `http://127.0.0.1:24050/`. The host fetches its
counter catalog, proxies backend HTTP resources, and opens backend WebSockets
for game data, subscriptions, and command relays. This game-data “telemetry” is
not a vosulay analytics upload. Sources: [`src/config.rs`](../src/config.rs),
[`src/api.rs`](../src/api.rs), [`src/backend.rs`](../src/backend.rs),
[`src/remote.rs`](../src/remote.rs).

Profile following adds one unauthenticated selected-field v2 socket to the
configured backend, reading only `/settings/mode`; it carries no broker/service
token. Automatic changes affect vosulay's own configuration, never osu!'s data.
The view-only active-profile-ID route cannot mutate state. Unpinned viewers
poll that ID; explicitly URL-selected profiles remain fixed. Editor fonts are
bundled and embedded, with no external font requests.

**Network access is not restricted to the configured backend.** Custom HTTP(S)
overlays and their scripts/resources can load unrelated origins directly; the
routing bootstrap only intercepts matching backend requests. Vosulay's own
requests stay with the backend the user configured: the counter catalog fetch
and the remote HTTP proxy both refuse to follow redirects. CEF runs with
`--disable-background-networking` and `--disable-component-update`, so
Chromium's background services (field-trial downloads, component updates and
similar) are off. CEF crash reporting is only enabled by a `crash_reporter.cfg`
file beside the executable
([CEF documentation](https://github.com/chromiumembedded/cef/blob/master/docs/crash_reporting.md));
no package ships one. The prebuilt CEF binaries themselves have not been
network-audited. Sources: [`assets/backend.js`](../assets/backend.js),
[`src/config.rs`](../src/config.rs), [`src/api.rs`](../src/api.rs),
[`src/remote.rs`](../src/remote.rs),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs).

The host attempts a remote HTTP listener at `127.0.0.1:5050` by default, plus a
separate loopback listener on an OS-assigned port for the in-game broker. The
remote listener starts with the host even without a viewer; failure to bind its
port does not disable the local broker. Settings can expose it to LAN interfaces.
There is **no authentication token for viewers and no listener TLS**. Loopback peers are
always allowed; other peers must match configured comma-separated exact IPs or
four-component IPv4 wildcard patterns. Host/Origin and Sec-Fetch-Site validation
are additional guards, not user authentication. Sources:
[`src/remote.rs`](../src/remote.rs), [`src/config.rs`](../src/config.rs).

Allowed viewers can select any configured profile, see its counters/game data,
retrieve readable resources under configured local overlay roots, and use the
proxied backend. HTTP accepts GET/HEAD, but supported WebSocket routes include
bidirectional command relays: “view-only profile” must not be interpreted as a
read-only backend security boundary. Restrict the listener and allowlist to
trusted clients/networks. Sources: [`src/remote.rs`](../src/remote.rs),
[`src/backend.rs`](../src/backend.rs).

**vosu broker token.** When `vosu_url` is loopback, vosulay reads vosu's
per-start `broker.json` token and presents it on upstream
`/websocket/commands` upgrades, which gives those sockets vosu's same-origin
write permission for counter settings. Only in-game counter sockets get it:
they must present a per-run broker secret that vosulay hands only to its own
CEF renderer (never over HTTP), come from a loopback peer, and carry exactly the
listener's own `Origin`. A program that reaches the broker port and claims
that origin, LAN viewers, other sites, local HTML files, and counters shown in
OBS or a browser do not get it, and it is never sent on other routes or to a
non-loopback backend. A file naming a wildcard listener (`0.0.0.0` or `[::]`, from
a vosu bound to every interface) is accepted only for a loopback `vosu_url` on
the same scheme and port; the token still goes only to that loopback address. The host also sends it itself on the editor's vosu
overlay settings and profile writes (`POST /api/counters/settings/<overlay>`,
`POST /api/counters/profiles`); the editor page names only the overlay,
profile and values, never the URL, and never sees the token. Anyone who can
use the in-game editor or the opt-in manage page can therefore change vosu
overlay settings and profiles. Details:
[integrations](integrations.md#vosu-broker-token). Sources:
[`src/backend/credential.rs`](../src/backend/credential.rs),
[`src/backend.rs`](../src/backend.rs), [`src/remote.rs`](../src/remote.rs),
[`src/api/vosu.rs`](../src/api/vosu.rs),
[`crates/browser-cef/src/bridge.rs`](../crates/browser-cef/src/bridge.rs),
[`assets/backend.js`](../assets/backend.js).

**Settings → Advanced → Allow editing on this computer** (off by default) serves
the full editor to loopback clients, and only under the Host
`manage.localhost:<port>` (accepted only on loopback listeners); the plain
listener address stays view-only. The manage origin serves nothing else: its
counters and local files load from the listener's own address, so counter
scripts, like other websites, fail the same-origin (Sec-Fetch-Site/Origin)
checks on its routes, and POSTs must be JSON. Page saves cannot introduce new
local-overlay capabilities unless the user chose the file in a native dialog
during this run, so a page script cannot widen local file access. The page sends
`frame-ancestors 'none'` and `X-Frame-Options: DENY`, and refuses `/overlay`
requests whose `Sec-Fetch-Dest` is not `document`, so a counter or website
cannot frame it to trick clicks into same-origin edits. Source:
[`src/remote/manage.rs`](../src/remote/manage.rs).

`vosulay --toggle-edit` and `--quit` reach the running instance only through a
per-user local channel, never TCP/HTTP. On Linux it is `control.sock` beside the
instance lock, mode 0600, and the listener drops peers whose `SO_PEERCRED` uid is
not its own. On Windows it is a named pipe whose name carries a per-user hash,
created with `PIPE_REJECT_REMOTE_CLIENTS`, `FILE_FLAG_FIRST_PIPE_INSTANCE` and
the default security descriptor (other users cannot write commands); if the
pipe cannot be created, vosulay starts without it. Command lines and replies are
bounded in size and time. Source: [`src/runtime/control.rs`](../src/runtime/control.rs).

Counters and custom web pages execute JavaScript in CEF's sandboxed renderer
infrastructure. The sandbox is not an outbound-network allowlist. Only
`vosulay://manager` receives the explicit Chromium local/loopback-network
permission grant; other origins/permissions retain Chromium defaults. The
application enables autoplay without a user gesture. Sources:
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs).

Local HTML selection creates a 128-bit capability URL under `vosulay-overlay://`.
The capability authorizes the selected file's **parent directory and descendants**,
not only that one HTML file. Every resource request resolves canonical paths,
rejecting traversal or symlink escape; readable regular files are served with a
32 MiB limit. Anyone able to retrieve the capability should be treated as able to
read resources under that root, including through the authorized remote server.
Keep private files out of overlay directories. Sources:
[`src/runtime/editor.rs`](../src/runtime/editor.rs),
[`src/shell_policy.rs`](../src/shell_policy.rs),
[`src/runtime/resources.rs`](../src/runtime/resources.rs),
[`crates/browser-cef/src/scheme.rs`](../crates/browser-cef/src/scheme.rs),
[`src/remote.rs`](../src/remote.rs).

Local schemes are secure contexts but overlays are not cross-origin isolated.
SharedArrayBuffer is unavailable by default. **Allow unrestricted
SharedArrayBuffer** is an application-wide compatibility opt-in requiring restart;
it enables Chromium's unrestricted-access feature for all overlays, weakening a
Spectre-related isolation protection. Use it only with fully trusted content.
Sources: [`crates/browser-cef/src/scheme.rs`](../crates/browser-cef/src/scheme.rs),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs),
[`src/config.rs`](../src/config.rs), [`src/runtime/host.rs`](../src/runtime/host.rs).

## Analytics, crash reporting, and updates

No vosulay-owned analytics uploader, automatic crash uploader, or self-update
mechanism was identified in the host startup/loop, backend, or CEF wrapper.
Renderer termination records an error locally and CEF writes a local `cef.log`.
Chromium's background services are switched off and CEF crash reporting is not
configured (see [Network exposure](#network-exposure-and-web-content)); the
internals of the prebuilt CEF and Windows injected DLLs have not been audited.
Third-party overlay scripts can collect/send data according to their own code.
Sources: [`src/runtime/host.rs`](../src/runtime/host.rs),
[`src/backend.rs`](../src/backend.rs),
[`crates/browser-cef/src/lib.rs`](../crates/browser-cef/src/lib.rs),
[`tools/cef/pins.json`](../tools/cef/pins.json).

Runtime startup uses already packaged/retained CEF assets rather than downloading
updates. Download/acquisition code belongs to build tooling. Installing a newer
release or using a distribution package manager is separate from host execution.
Sources: [`src/main.rs`](../src/main.rs), [`src/runtime.rs`](../src/runtime.rs),
[`crates/cef-tools/src/lib.rs`](../crates/cef-tools/src/lib.rs),
[`packaging/linux/build.sh`](../packaging/linux/build.sh).

## Reporting vulnerabilities

Report vulnerabilities privately as described in
[`SECURITY.md`](../SECURITY.md). Do not publish exploit details or private data
in a public issue.

## Open questions for maintainers/reviewers

- Native installation/uninstallation and third-party Windows DLL internals have
  not been audited by executing them for this document. Sources under review:
  [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss),
  [`tools/cef/pins.json`](../tools/cef/pins.json).
