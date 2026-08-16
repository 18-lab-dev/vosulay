use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const QT_MODULES: &[&str] = &[
    "Qt6WebEngineWidgets",
    "Qt6Core",
    "Qt6Widgets",
    "Qt6WebChannel",
    "Qt6Network",
    "Qt6DBus",
];

const AUTO_WEBGPU_PREFIX_ENV: &str = "VOSULAY_WEBGPU_QT_PREFIX";
const SKIP_AUTO_WEBGPU_BUILD_ENV: &str = "VOSULAY_SKIP_AUTO_WEBGPU_BUILD";

const QTWEBENGINE_RUNTIME_ENVIRONMENT: &[&str] = &[
    "QTWEBENGINEPROCESS_PATH",
    "QTWEBENGINE_RESOURCES_PATH",
    "QTWEBENGINE_LOCALES_PATH",
];

const CMAKE_ENVIRONMENT: &[&str] = &[
    "CMAKE",
    "CMAKE_BUILD_TYPE",
    "CMAKE_FIND_ROOT_PATH",
    "CMAKE_GENERATOR",
    "CMAKE_GENERATOR_PLATFORM",
    "CMAKE_GENERATOR_TOOLSET",
    "CMAKE_PREFIX_PATH",
    "CMAKE_SYSROOT",
    "CMAKE_TOOLCHAIN_FILE",
    "LayerShellQt_DIR",
    "LayerShellQt_ROOT",
    "Qt6_DIR",
    "Qt6_ROOT",
];

struct LayerShellQt {
    include_paths: Vec<PathBuf>,
    library: PathBuf,
}

struct QtWebEngineMetadata {
    prefix: PathBuf,
    libdir: PathBuf,
    libexecdir: PathBuf,
}

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/qt/bridge.h");
    println!("cargo:rerun-if-changed=src/qt/qt_shell.h");
    println!("cargo:rerun-if-changed=src/qt/qt_shell.cpp");
    println!("cargo:rerun-if-changed=assets/vosulay.qrc");
    println!("cargo:rerun-if-changed=assets/branding/vosulay-app-icon.png");
    println!("cargo:rerun-if-env-changed=QT_MOC");
    println!("cargo:rerun-if-env-changed=QT_RCC");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed={AUTO_WEBGPU_PREFIX_ENV}");
    println!("cargo:rerun-if-env-changed={SKIP_AUTO_WEBGPU_BUILD_ENV}");
    println!("cargo:rerun-if-changed=extra/build-webgpu-qt.sh");
    println!("cargo:rerun-if-changed=extra/qtwebengine-6.11-webgpu.patch");
    for variable in QTWEBENGINE_RUNTIME_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    for variable in CMAKE_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    ensure_dawn_webengine_available();

    let webengine_core = probe_webengine_core().unwrap_or_else(|error| {
        panic!("Qt 6.8 or newer with the Qt6WebEngineCore development package is required: {error}")
    });
    let webengine = verify_webengine_dawn(&webengine_core);

    let mut qt_include_paths = Vec::new();
    for module in QT_MODULES {
        let library = pkg_config::Config::new()
            .atleast_version("6.8")
            .cargo_metadata(true)
            .probe(module)
            .unwrap_or_else(|error| {
                panic!("Qt 6.8 or newer with the {module} development package is required: {error}")
            });
        qt_include_paths.extend(library.include_paths);
    }
    let webengine_process_path = resolve_webengine_path(
        "QTWEBENGINEPROCESS_PATH",
        webengine.libexecdir.join("QtWebEngineProcess"),
    );
    let webengine_resources_path = resolve_webengine_path(
        "QTWEBENGINE_RESOURCES_PATH",
        webengine.prefix.join("share/qt6/resources"),
    );
    let webengine_locales_path = resolve_webengine_path(
        "QTWEBENGINE_LOCALES_PATH",
        webengine
            .prefix
            .join("share/qt6/translations/qtwebengine_locales"),
    );
    let webengine_process_literal = cxx_string_literal(&webengine_process_path);
    let webengine_resources_literal = cxx_string_literal(&webengine_resources_path);
    let webengine_locales_literal = cxx_string_literal(&webengine_locales_path);

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    let layer_shell_qt = discover_layer_shell_qt(&out_dir);
    qt_include_paths.extend(layer_shell_qt.include_paths);
    let mut deduplicated_include_paths = Vec::with_capacity(qt_include_paths.len());
    for include_path in qt_include_paths {
        if !deduplicated_include_paths.contains(&include_path) {
            deduplicated_include_paths.push(include_path);
        }
    }
    let qt_include_paths = deduplicated_include_paths;

    let moc_output = out_dir.join("moc_bridge.cpp");
    let moc = env::var_os("QT_MOC").unwrap_or_else(|| {
        pkg_config::get_variable("Qt6Core", "libexecdir")
            .map(PathBuf::from)
            .map(|directory| directory.join("moc").into_os_string())
            .unwrap_or_else(|_| OsString::from("moc"))
    });
    let mut moc_command = Command::new(moc);
    for include_path in &qt_include_paths {
        moc_command.arg("-I").arg(include_path);
    }
    let moc_status = moc_command
        .arg("-o")
        .arg(&moc_output)
        .arg("src/qt/bridge.h")
        .status()
        .expect("failed to execute Qt moc");
    assert!(moc_status.success(), "Qt moc failed with {moc_status}");

    let rcc_output = out_dir.join("qrc_vosulay.cpp");
    let rcc = env::var_os("QT_RCC").unwrap_or_else(|| {
        pkg_config::get_variable("Qt6Core", "libexecdir")
            .map(PathBuf::from)
            .map(|directory| directory.join("rcc").into_os_string())
            .unwrap_or_else(|_| OsString::from("rcc"))
    });
    let rcc_status = Command::new(rcc)
        .arg("-name")
        .arg("vosulay")
        .arg("-o")
        .arg(&rcc_output)
        .arg("assets/vosulay.qrc")
        .status()
        .expect("failed to execute Qt rcc");
    assert!(rcc_status.success(), "Qt rcc failed with {rcc_status}");

    let mut build = cxx_build::bridge("src/ffi.rs");
    build
        .file("src/qt/qt_shell.cpp")
        .file(&moc_output)
        .file(&rcc_output)
        .include("src/qt")
        .std("c++17")
        .flag_if_supported("-fPIC")
        .define(
            "VOSULAY_QTWEBENGINEPROCESS_PATH",
            webengine_process_literal.as_str(),
        )
        .define(
            "VOSULAY_QTWEBENGINE_RESOURCES_PATH",
            webengine_resources_literal.as_str(),
        )
        .define(
            "VOSULAY_QTWEBENGINE_LOCALES_PATH",
            webengine_locales_literal.as_str(),
        );
    for include_path in &qt_include_paths {
        build.include(include_path);
    }
    build.compile("vosulay-qt-shell");

    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        webengine.libdir.to_string_lossy()
    );
    println!(
        "cargo:rustc-link-arg={}",
        layer_shell_qt.library.to_string_lossy()
    );
}

fn probe_webengine_core() -> Result<pkg_config::Library, pkg_config::Error> {
    pkg_config::Config::new()
        .atleast_version("6.8")
        .cargo_metadata(false)
        .probe("Qt6WebEngineCore")
}

fn find_webengine_config_header(webengine_core: &pkg_config::Library) -> Option<PathBuf> {
    const HEADER: &str = "qtwebenginecore-config.h";
    webengine_core
        .include_paths
        .iter()
        .flat_map(|include_path| {
            [
                include_path.join(HEADER),
                include_path.join("QtWebEngineCore").join(HEADER),
            ]
        })
        .find(|candidate| candidate.is_file())
}

fn config_header_has_dawn(config_header: &Path) -> bool {
    fs::read_to_string(config_header)
        .map(|config| {
            config
                .lines()
                .any(|line| line.trim() == "#define QT_FEATURE_webengine_dawn 1")
        })
        .unwrap_or(false)
}

/// True when the currently configured `PKG_CONFIG_PATH` already resolves a
/// Qt WebEngine build with Dawn WebGPU support compiled in.
fn webengine_probe_has_dawn() -> bool {
    probe_webengine_core().is_ok_and(|webengine_core| {
        find_webengine_config_header(&webengine_core)
            .is_some_and(|header| config_header_has_dawn(&header))
    })
}

/// Directory `extra/build-webgpu-qt.sh` installs an auto-built Dawn-enabled
/// Qt WebEngine into, unless overridden by `VOSULAY_WEBGPU_QT_PREFIX`.
fn auto_webgpu_prefix() -> PathBuf {
    if let Some(value) = env::var_os(AUTO_WEBGPU_PREFIX_ENV) {
        return PathBuf::from(value);
    }
    let cache_dir = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| {
            panic!(
                "could not determine a cache directory for the auto-built Qt WebEngine; \
                 set XDG_CACHE_HOME, HOME, or {AUTO_WEBGPU_PREFIX_ENV}"
            )
        });
    cache_dir.join("vosulay").join("qt-webgpu")
}

/// Prepends `prefix/lib/pkgconfig` to `PKG_CONFIG_PATH` for the remainder of
/// this build script invocation, so a previously auto-built (or externally
/// pre-built) Dawn Qt WebEngine at `prefix` is discoverable by pkg-config.
fn prepend_pkg_config_path(prefix: &Path) {
    let mut paths = vec![prefix.join("lib").join("pkgconfig")];
    if let Some(existing) = env::var_os("PKG_CONFIG_PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let joined =
        env::join_paths(paths).expect("failed to join PKG_CONFIG_PATH entries into one value");
    // SAFETY: build scripts run single-threaded before any subprocess is spawned that
    // would read this variable concurrently with this write.
    unsafe {
        env::set_var("PKG_CONFIG_PATH", joined);
    }
}

fn run_webgpu_build_script(prefix: &Path) {
    let manifest_dir =
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo did not set CARGO_MANIFEST_DIR");
    let script = PathBuf::from(manifest_dir)
        .join("extra")
        .join("build-webgpu-qt.sh");
    let status = Command::new("sh")
        .arg(&script)
        .arg(prefix)
        .status()
        .unwrap_or_else(|error| panic!("failed to execute {}: {error}", script.display()));
    if !status.success() {
        panic!(
            "{} failed with {status}; see the build output above for the underlying error, \
             or set {SKIP_AUTO_WEBGPU_BUILD_ENV}=1 and build/select a Dawn-enabled Qt WebEngine \
             yourself with extra/build-webgpu-qt.sh",
            script.display()
        );
    }
}

/// Ensures a Dawn WebGPU-enabled Qt WebEngine is discoverable via pkg-config
/// before the rest of `main` probes for it. Order of preference:
/// 1. Whatever `PKG_CONFIG_PATH`/system pkg-config already resolves.
/// 2. A Dawn Qt WebEngine previously auto-built into the cache prefix.
/// 3. A freshly auto-built Dawn Qt WebEngine, unless explicitly skipped via
///    `VOSULAY_SKIP_AUTO_WEBGPU_BUILD`, in which case the later authoritative
///    check in `verify_webengine_dawn` panics with manual build instructions.
fn ensure_dawn_webengine_available() {
    if webengine_probe_has_dawn() {
        return;
    }

    let prefix = auto_webgpu_prefix();
    prepend_pkg_config_path(&prefix);
    if webengine_probe_has_dawn() {
        return;
    }

    if env::var_os(SKIP_AUTO_WEBGPU_BUILD_ENV).is_some() {
        return;
    }

    println!(
        "cargo:warning=vosulay: no Dawn WebGPU-enabled Qt WebEngine was found; building one \
         automatically into {} (first build only). This compiles Qt WebEngine/Chromium from \
         source and can take 20-40+ minutes depending on hardware. Set \
         {SKIP_AUTO_WEBGPU_BUILD_ENV}=1 to skip this and build/select a Dawn-enabled Qt \
         WebEngine yourself with extra/build-webgpu-qt.sh instead.",
        prefix.display()
    );
    run_webgpu_build_script(&prefix);
    println!("cargo:warning=vosulay: Dawn WebGPU-enabled Qt WebEngine build finished.");
}

fn verify_webengine_dawn(webengine_core: &pkg_config::Library) -> QtWebEngineMetadata {
    let config_header = find_webengine_config_header(webengine_core).unwrap_or_else(|| {
        panic!(
            "Qt WebEngine's public config header was not found; build and select the Dawn-enabled Qt with extra/build-webgpu-qt.sh"
        )
    });
    if !config_header_has_dawn(&config_header) {
        panic!(
            "Qt WebEngine was built without Dawn WebGPU support; build and select it with extra/build-webgpu-qt.sh \
             (or unset {SKIP_AUTO_WEBGPU_BUILD_ENV} to let the build do this automatically)"
        );
    }

    QtWebEngineMetadata {
        prefix: pkg_config_path("Qt6WebEngineCore", "prefix"),
        libdir: pkg_config_path("Qt6WebEngineCore", "libdir"),
        libexecdir: pkg_config_path("Qt6WebEngineCore", "libexecdir"),
    }
}

fn pkg_config_path(package: &str, variable: &str) -> PathBuf {
    let value = pkg_config::get_variable(package, variable).unwrap_or_else(|error| {
        panic!("pkg-config package {package} did not provide {variable}: {error}")
    });
    if value.is_empty() {
        panic!("pkg-config package {package} provided an empty {variable}");
    }
    PathBuf::from(value)
}

fn resolve_webengine_path(variable: &str, default_path: PathBuf) -> PathBuf {
    let path = env::var_os(variable)
        .filter(|value| !value.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_path);
    let (valid, expected) = match variable {
        "QTWEBENGINEPROCESS_PATH" => (path.is_file(), "an executable file"),
        "QTWEBENGINE_RESOURCES_PATH" | "QTWEBENGINE_LOCALES_PATH" => (path.is_dir(), "a directory"),
        _ => unreachable!("unknown Qt WebEngine runtime path variable: {variable}"),
    };
    if !valid {
        panic!(
            "Qt WebEngine runtime path {variable} must point to {expected}, but {} does not",
            path.display()
        );
    }
    path
}

fn cxx_string_literal(path: &Path) -> String {
    let mut literal = String::from("\"");
    for byte in path.to_string_lossy().as_bytes() {
        match *byte {
            b'\\' => literal.push_str("\\\\"),
            b'"' => literal.push_str("\\\""),
            b'\n' => literal.push_str("\\n"),
            b'\r' => literal.push_str("\\r"),
            b'\t' => literal.push_str("\\t"),
            0x20..=0x7e => literal.push(*byte as char),
            byte => literal.push_str(&format!("\\{byte:03o}")),
        }
    }
    literal.push('"');
    literal
}

fn discover_layer_shell_qt(out_dir: &Path) -> LayerShellQt {
    let source_dir = out_dir.join("layershellqt-cmake-probe");
    let build_dir = out_dir.join("layershellqt-cmake-build");
    fs::create_dir_all(&source_dir)
        .unwrap_or_else(|error| panic!("failed to create LayerShellQt CMake probe: {error}"));
    if build_dir.exists() {
        fs::remove_dir_all(&build_dir)
            .unwrap_or_else(|error| panic!("failed to reset LayerShellQt CMake probe: {error}"));
    }

    let cmake_lists = source_dir.join("CMakeLists.txt");
    fs::write(
        &cmake_lists,
        r#"cmake_minimum_required(VERSION 3.20)
project(vosulay_layershellqt_probe LANGUAGES CXX)
find_package(LayerShellQt CONFIG REQUIRED)
if(NOT TARGET LayerShellQt::Interface)
    message(FATAL_ERROR "LayerShellQt package does not provide LayerShellQt::Interface")
endif()
file(GENERATE
    OUTPUT "${CMAKE_BINARY_DIR}/layershellqt-$<CONFIG>.txt"
    CONTENT "include=$<JOIN:$<TARGET_PROPERTY:LayerShellQt::Interface,INTERFACE_INCLUDE_DIRECTORIES>,\ninclude=>\nlibrary=$<TARGET_LINKER_FILE:LayerShellQt::Interface>\n"
)
"#,
    )
    .unwrap_or_else(|error| panic!("failed to write LayerShellQt CMake probe: {error}"));

    let build_type = env::var("CMAKE_BUILD_TYPE").unwrap_or_else(|_| {
        if env::var_os("DEBUG").as_deref() == Some(std::ffi::OsStr::new("true")) {
            "Debug".to_owned()
        } else {
            "Release".to_owned()
        }
    });
    let cmake = env::var_os("CMAKE").unwrap_or_else(|| OsString::from("cmake"));
    let mut command = Command::new(cmake);
    command
        .arg("-S")
        .arg(&source_dir)
        .arg("-B")
        .arg(&build_dir)
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"));
    for variable in [
        "CMAKE_TOOLCHAIN_FILE",
        "CMAKE_SYSROOT",
        "CMAKE_FIND_ROOT_PATH",
        "LayerShellQt_DIR",
        "Qt6_DIR",
    ] {
        if let Some(value) = env::var_os(variable) {
            command.arg(format!("-D{variable}={}", value.to_string_lossy()));
        }
    }
    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "failed to execute CMake while locating LayerShellQt::Interface; \
             install CMake and the LayerShellQt development package: {error}"
        )
    });
    if !output.status.success() {
        panic!(
            "could not locate the LayerShellQt::Interface CMake target; install the \
             LayerShellQt development package and set CMAKE_PREFIX_PATH or \
             CMAKE_TOOLCHAIN_FILE for non-system prefixes.\nCMake stdout:\n{}\nCMake stderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata = select_layer_shell_metadata(&build_dir, &build_type);
    parse_layer_shell_metadata(&metadata)
}

fn select_layer_shell_metadata(build_dir: &Path, build_type: &str) -> PathBuf {
    let preferred = build_dir.join(format!("layershellqt-{build_type}.txt"));
    if preferred.is_file() {
        return preferred;
    }

    let mut candidates = fs::read_dir(build_dir)
        .unwrap_or_else(|error| panic!("failed to read LayerShellQt CMake probe output: {error}"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("layershellqt-") && name.ends_with(".txt"))
        })
        .collect::<Vec<_>>();
    if let Some(position) = candidates.iter().position(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.eq_ignore_ascii_case(&format!("layershellqt-{build_type}.txt"))
            })
    }) {
        return candidates.swap_remove(position);
    }
    candidates.sort();
    candidates.into_iter().next().unwrap_or_else(|| {
        panic!("LayerShellQt CMake probe did not produce imported target metadata")
    })
}

fn parse_layer_shell_metadata(metadata: &Path) -> LayerShellQt {
    let contents = fs::read_to_string(metadata)
        .unwrap_or_else(|error| panic!("failed to read LayerShellQt target metadata: {error}"));
    let mut include_paths = Vec::new();
    let mut library = None;
    for line in contents.lines() {
        if let Some(path) = line.strip_prefix("include=") {
            if !path.is_empty() {
                include_paths.push(PathBuf::from(path));
            }
        } else if let Some(path) = line.strip_prefix("library=")
            && !path.is_empty()
        {
            library = Some(PathBuf::from(path));
        }
    }

    if include_paths.is_empty() {
        panic!("LayerShellQt::Interface did not publish any include directories");
    }
    for include_path in &include_paths {
        if !include_path.is_dir() {
            panic!(
                "LayerShellQt::Interface published a missing include directory: {}",
                include_path.display()
            );
        }
    }
    let library = library
        .unwrap_or_else(|| panic!("LayerShellQt::Interface did not publish an imported library"));
    if !library.is_file() {
        panic!(
            "LayerShellQt::Interface published a missing imported library: {}",
            library.display()
        );
    }

    LayerShellQt {
        include_paths,
        library,
    }
}
