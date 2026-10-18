#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Linux ]] || ! systemd-detect-virt --vm >/dev/null 2>&1; then
  echo "REFUSED: workload requires the Linux VM" >&2
  exit 90
fi
if [[ $# -ne 6 ]]; then
  echo "usage: linux_guest.sh SOURCE_SHA256 INPUTS_SHA256 CPU_SECONDS DELIVERY_SECONDS WARMUP REPEATS" >&2
  exit 2
fi
source_digest="$1"
inputs_digest="$2"
[[ "$source_digest" =~ ^[a-f0-9]{64}$ && "$inputs_digest" =~ ^[a-f0-9]{64}$ ]] || exit 2
root="$(cd -- "$(dirname -- "$0")" && pwd -P)"
cd "$root"
printf '%s  %s\n' "$source_digest" source.tar.gz "$inputs_digest" inputs.tar.gz | sha256sum --check --strict
source_dir="$root/snapshots/$source_digest"
cache="$root/cache/$inputs_digest"
mkdir -p "$source_dir" "$cache" "$root/reports"
if [[ ! -f "$source_dir/.complete" ]]; then
  tar -xzf source.tar.gz -C "$source_dir"
  touch "$source_dir/.complete"
fi
if [[ ! -f "$cache/.complete" ]]; then
  tar -xzf inputs.tar.gz -C "$cache"
  touch "$cache/.complete"
fi
# Archive metadata is normalized. The shared Cargo target must see this
# snapshot's inputs as newer than its previous dep-info, even on a revisit.
find "$source_dir" -type f -exec touch -- {} +
cd "$source_dir"
export CEF_PATH="$cache/cef/cef_binary_154.0.34+g14c5a08+chromium-154.0.8037.98_linux64_minimal"
export CARGO_HOME="$cache/cargo" CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR="$root/target"
export PYTHONPATH="$cache/pillow"
export PATH="$HOME/.cargo/bin:$PATH"
weston="$cache/weston/usr"
[[ -f "$CEF_PATH/.vosulay-cef-verified.json" && -x "$weston/bin/weston" ]] || exit 91
for command in cargo rustc cc pkg-config bwrap; do
  command -v "$command" >/dev/null || { echo "PREREQUISITE: guest tool missing: $command" >&2; exit 92; }
done
# The existing benchmark's binary staging uses target/release. Keep its frozen
# paths and build output consistent by linking this snapshot's target to the
# shared task-owned Cargo target, before its first build.
mkdir -p "$CARGO_TARGET_DIR"
[[ -e target ]] || ln -s "$CARGO_TARGET_DIR" target
"$cache/python/cpython-3.13.13-linux-x86_64-gnu/bin/python3.13" \
  tools/autoresearch/benchmark.py --cef-dir "$CEF_PATH" --weston "$weston" \
  --output "$root/reports" --cpu-seconds "$3" --delivery-seconds "$4" --warmup "$5" --repeats "$6"
