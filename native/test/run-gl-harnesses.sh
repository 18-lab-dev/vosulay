#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${TMPDIR:-/tmp}/vosulay-gl-harness-$$
target_pid=
host_pid=
cleanup() {
    if [ -n "$target_pid" ]; then kill "$target_pid" 2>/dev/null || :; wait "$target_pid" 2>/dev/null || :; fi
    if [ -n "$host_pid" ]; then kill "$host_pid" 2>/dev/null || :; wait "$host_pid" 2>/dev/null || :; fi
    rm -rf "$out"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM
mkdir -p "$out"
cc -O2 -Wall -Wextra "$root/test/glx-state-harness.c" -o "$out/glx-state-harness" -lX11 -lGL -ldl
cc -O2 -Wall -Wextra "$root/test/egl-state-harness.c" -o "$out/egl-state-harness" -lEGL -lGLESv2 -ldl
lib=${VOSULAY_AGENT_GL:-$root/target/release/liboverlay_agent_gl.so}
host=${VOSULAY_SHM_HOST:-$root/target/release/examples/shm-checkerboard}
if [ ! -r "$lib" ]; then echo "build the cdylib first: cargo build --manifest-path $root/Cargo.toml -p overlay-agent-gl --release" >&2; exit 2; fi
if [ ! -x "$host" ]; then echo "build the SHM host first: cargo build --manifest-path $root/Cargo.toml -p overlay-host --example shm-checkerboard --release" >&2; exit 2; fi

run_case() {
    name=$1
    target=$2
    credentials="$out/$name.credentials"
    env -u VOSULAY_AGENT_SOCKET -u VOSULAY_AGENT_COOKIE \
        LD_PRELOAD="$lib${LD_PRELOAD:+:$LD_PRELOAD}" \
        "$target" "$credentials" >"$out/$name.log" 2>&1 &
    target_pid=$!
    "$host" "$target_pid" "$credentials" >"$out/$name.host.log" 2>&1 &
    host_pid=$!
    status=0
    wait "$target_pid" || status=$?
    kill "$host_pid" 2>/dev/null || :
    wait "$host_pid" || :
    target_pid=
    host_pid=
    if [ "$status" -ne 0 ]; then
        cat "$out/$name.log" "$out/$name.host.log" >&2
        return "$status"
    fi
    echo "$name SHM compositor passed"
}

run_case glx "$out/glx-state-harness"
run_case egl "$out/egl-state-harness"
