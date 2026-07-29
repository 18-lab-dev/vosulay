#!/bin/sh
set -eu

QT_VERSION=6.11.1
QT_COMMIT=eb0793cc4b76e93cf669f586fd68c76019f40ec9
QT_STABILITY_COMMITS='313960be3a586969c3af27f1845ab47d9b40ac34 ae6991d950bb343accad96b96047fe605c17bcc3'
QT_GITLINK_COMMIT=58c11ad487f8a237cf0ac71cc3e818b52db150df
CHROMIUM_COMMIT=37b6aeaa3ef9bf7e1901aa02a317a2707557709d
QT_REPOSITORY=https://github.com/qt/qtwebengine.git

usage() {
    printf 'Usage: %s PREFIX [CACHE_DIR]\n' "$0" >&2
    printf 'Builds Qt WebEngine %s with Dawn into PREFIX.\n' "$QT_VERSION" >&2
    exit 2
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "required tool '$1' was not found in PATH"
}

shell_quote() {
    escaped=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
    printf "'%s'" "$escaped"
}
qt_install_relative() {
    install_path=$("$QT_PATHS" --query "$1") ||
        fail "qtpaths could not query $1"
    case "$install_path" in
        "$qt_prefix"/*) printf '%s' "${install_path#"$qt_prefix"/}" ;;
        *) fail "$1 path $install_path is outside the Qt prefix $qt_prefix" ;;
    esac
}


[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
[ -n "$1" ] || usage

PREFIX=$1
CACHE_DIR=${2:-${XDG_CACHE_HOME:-${HOME:?HOME must be set when CACHE_DIR is omitted}}/vosulay/qtwebengine-$QT_VERSION}

for tool in git cmake ninja pkg-config grep sed gperf python3; do
    need_tool "$tool"
done

for module in Qt6Core Qt6Gui Qt6Network Qt6Widgets Qt6Quick Qt6WebChannel; do
    module_version=$(pkg-config --modversion "$module" 2>/dev/null) ||
        fail "$module development files were not found through pkg-config"
    [ "$module_version" = "$QT_VERSION" ] ||
        fail "Qt $QT_VERSION is required, but pkg-config selected $module $module_version"
done

qt_bin_dir=$(pkg-config --variable=bindir Qt6Core)
QT_PATHS=$qt_bin_dir/qtpaths
[ -x "$QT_PATHS" ] || fail "Qt's qtpaths helper was not found at $QT_PATHS"
PYTHON=$(command -v python3)
"$PYTHON" -c 'import html5lib' >/dev/null 2>&1 ||
    fail "Python module html5lib is required by Qt WebEngine"

qt_prefix=$(pkg-config --variable=prefix Qt6Core)
[ -n "$qt_prefix" ] || fail "Qt6Core.pc does not declare its installation prefix"

mkdir -p "$PREFIX" "$CACHE_DIR"
PREFIX=$(CDPATH= cd "$PREFIX" && pwd -P)
CACHE_DIR=$(CDPATH= cd "$CACHE_DIR" && pwd -P)
if [ -d "$qt_prefix" ]; then
    qt_prefix=$(CDPATH= cd "$qt_prefix" && pwd -P)
fi
qt_include_dir=$(pkg-config --variable=includedir Qt6Core)
case "$qt_include_dir" in
    "$qt_prefix"/*) qt_include_rel=${qt_include_dir#"$qt_prefix"/} ;;
    *) fail "Qt6Core include directory $qt_include_dir is outside its prefix $qt_prefix" ;;
esac
qt_libexec_rel=$(qt_install_relative QT_INSTALL_LIBEXECS)
qt_data_rel=$(qt_install_relative QT_INSTALL_DATA)
qt_translations_rel=$(qt_install_relative QT_INSTALL_TRANSLATIONS)
[ "$PREFIX" != "$qt_prefix" ] || \
    fail "PREFIX must not overwrite the installed Qt at $qt_prefix"

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
PATCH=$SCRIPT_DIR/qtwebengine-6.11-webgpu.patch
[ -f "$PATCH" ] || fail "patch not found: $PATCH"

SOURCE_DIR=$CACHE_DIR/source
BUILD_DIR=$CACHE_DIR/build
if [ ! -d "$SOURCE_DIR/.git" ]; then
    [ ! -e "$SOURCE_DIR" ] || fail "$SOURCE_DIR exists but is not a Git checkout"
    git clone --filter=blob:none --no-checkout "$QT_REPOSITORY" "$SOURCE_DIR"
fi

git -C "$SOURCE_DIR" fetch --depth 1 origin "refs/tags/v$QT_VERSION"
tag_commit=$(git -C "$SOURCE_DIR" rev-parse 'FETCH_HEAD^{commit}')
[ "$tag_commit" = "$QT_COMMIT" ] ||
    fail "Qt WebEngine tag v$QT_VERSION resolved to $tag_commit; expected $QT_COMMIT"
git -C "$SOURCE_DIR" checkout --detach --force "$QT_COMMIT"
git -C "$SOURCE_DIR" reset --hard "$QT_COMMIT"

for commit in $QT_STABILITY_COMMITS; do
    git -C "$SOURCE_DIR" fetch --depth 2 origin "$commit"
    git -C "$SOURCE_DIR" cherry-pick --no-commit "$commit"
done

git -C "$SOURCE_DIR" apply --check "$PATCH" >/dev/null 2>&1 ||
    fail "the Dawn patch does not apply cleanly after the pinned Qt stability commits"
git -C "$SOURCE_DIR" apply "$PATCH"

gitlink_commit=$(git -C "$SOURCE_DIR" rev-parse "$QT_COMMIT:src/3rdparty")
[ "$gitlink_commit" = "$QT_GITLINK_COMMIT" ] ||
    fail "pinned Qt commit records Chromium $gitlink_commit; expected $QT_GITLINK_COMMIT"

git -C "$SOURCE_DIR" submodule sync -- src/3rdparty
git -C "$SOURCE_DIR" submodule update --init --depth 1 -- src/3rdparty
git -C "$SOURCE_DIR/src/3rdparty" fetch --depth 1 origin "$CHROMIUM_COMMIT"
git -C "$SOURCE_DIR/src/3rdparty" checkout --detach --force "$CHROMIUM_COMMIT"
chromium_commit=$(git -C "$SOURCE_DIR/src/3rdparty" rev-parse HEAD)
[ "$chromium_commit" = "$CHROMIUM_COMMIT" ] ||
    fail "Chromium checkout is $chromium_commit; expected $CHROMIUM_COMMIT"

mkdir -p "$BUILD_DIR"
CMAKE_PREFIX_PATH="$qt_prefix${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}" \
    cmake -S "$SOURCE_DIR" -B "$BUILD_DIR" \
        -G Ninja \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" \
        -DCMAKE_INSTALL_LIBDIR=lib \
        -DPython3_EXECUTABLE="$PYTHON" \
        -DQT_BUILD_EXAMPLES=OFF \
        -DQT_BUILD_TESTS=OFF \
        -DQT_SHOW_EXTRA_IDE_SOURCES=OFF \
        -DFEATURE_webengine_dawn=ON \
        -DFEATURE_webengine_vulkan=ON \
        -DFEATURE_qtwebengine_build=ON \
        -DFEATURE_qtwebengine_widgets_build=ON \
        -DFEATURE_qtwebengine_quick_build=OFF \
        -DFEATURE_qtpdf_build=OFF

cmake --build "$BUILD_DIR" --parallel
cmake --install "$BUILD_DIR"

config_header=$PREFIX/$qt_include_rel/QtWebEngineCore/qtwebenginecore-config.h
[ -f "$config_header" ] || fail "installed WebEngine config header is missing: $config_header"
grep -Eq '^#define[[:space:]]+QT_FEATURE_webengine_dawn[[:space:]]+1[[:space:]]*$' \
    "$config_header" || fail "installed WebEngine config header does not advertise Dawn"
process_path=$PREFIX/$qt_libexec_rel/QtWebEngineProcess
resources_path=$PREFIX/$qt_data_rel/resources
locales_path=$PREFIX/$qt_translations_rel/qtwebengine_locales
[ -x "$process_path" ] || fail "installed QtWebEngineProcess is missing: $process_path"
[ -f "$resources_path/qtwebengine_resources.pak" ] ||
    fail "installed WebEngine resources are missing: $resources_path"
[ -d "$locales_path" ] || fail "installed WebEngine locales are missing: $locales_path"

pkg_config_path=$(shell_quote "$PREFIX/lib/pkgconfig")
cmake_prefix_path=$(shell_quote "$PREFIX")
library_path=$(shell_quote "$PREFIX/lib")
process_path=$(shell_quote "$process_path")
resources_path=$(shell_quote "$resources_path")
locales_path=$(shell_quote "$locales_path")
printf '\nUse this Qt WebEngine build for vosulay:\n'
printf 'export PKG_CONFIG_PATH=%s"${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"\n' "$pkg_config_path"
printf 'export CMAKE_PREFIX_PATH=%s"${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}"\n' "$cmake_prefix_path"
printf 'export LD_LIBRARY_PATH=%s"${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\n' "$library_path"
printf 'export QTWEBENGINEPROCESS_PATH=%s\n' "$process_path"
printf 'export QTWEBENGINE_RESOURCES_PATH=%s\n' "$resources_path"
printf 'export QTWEBENGINE_LOCALES_PATH=%s\n' "$locales_path"
