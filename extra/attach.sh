#!/bin/sh

usage() {
    printf 'Usage: %s x y width height\n' "$0" >&2
    exit 2
}

[ "$#" -eq 4 ] || usage

for value in "$@"; do
    case "$value" in
        ''|*[!0-9]*) usage ;;
    esac
done

exec vosulay --ipc-geometry "$1,$2,$3,$4"
