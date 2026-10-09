#!/usr/bin/env bash
# Builds release binaries and the Linux release artifacts:
#   dist/vosulay-<version>-linux-x86_64.tar.gz
#   dist/vosulay_<version>-1_amd64.deb, vosulay-<version>-1.x86_64.rpm,
#   vosulay-<version>-1-x86_64.pkg.tar.zst
# Run it in the Ubuntu 22.04 build environment (packaging/linux/Containerfile or
# the release workflow) so the binaries keep a glibc 2.35 floor. Requires nfpm
# on PATH or in $NFPM.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
out="${1:-dist}"
nfpm="${NFPM:-nfpm}"
target="${CARGO_TARGET_DIR:-target}"
version="$(cargo pkgid --locked -p vosulay | sed 's/.*[#@]//')"
name="vosulay-$version-linux-x86_64"
stage="$out/$name"
pkgroot="$out/.pkgroot"

if [ -e "$stage" ] || [ -e "$pkgroot" ]; then
    echo "remove $stage and $pkgroot first" >&2
    exit 1
fi
mkdir -p "$out"

# Packaged binaries find libcef beside themselves, never in a build cache.
export VOSULAY_PACKAGE_BUILD=1
# The overlay libraries are also build dependencies of vosulay; building them
# in the same invocation collides on their output filenames.
cargo build --release --locked -p vosulay
cargo build --release --locked -p vosulay-overlay -p vosulay-lookup
cef="$(cargo run --quiet --release --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
cargo run --quiet --release --locked -p vosulay-cef-tools -- package --platform linux-x86_64 \
    --cef-path "$cef" --output "$stage" \
    --exe "$target/release/vosulay" \
    --overlay "$target/release/libvosulay_overlay.so" \
    --overlay "$target/release/libvosulay_lookup.so" \
    --overlay crates/overlay-linux/VkLayer_vosulay_overlay.json

tar -C "$out" -czf "$out/$name.tar.gz" "$name"

# nfpm cannot override one file inside a tree, so the setuid sandbox is staged
# beside the library tree and installed with its own mode.
mkdir -p "$pkgroot"
cp -a "$stage" "$pkgroot/lib"
mv "$pkgroot/lib/chrome-sandbox" "$pkgroot/chrome-sandbox"
# nfpm expands environment variables in metadata but not in content paths.
sed "s|\${PKGROOT}|$(cd "$pkgroot" && pwd)|g" packaging/linux/nfpm.yaml >"$pkgroot/nfpm.yaml"
for packager in deb rpm archlinux; do
    VERSION="$version" "$nfpm" package \
        --config "$pkgroot/nfpm.yaml" --packager "$packager" --target "$out/"
done
rm -rf "$pkgroot"
ls -l "$out"
