use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const QT_MODULES: &[&str] = &[
    "Qt6Core",
    "Qt6Widgets",
    "Qt6WebEngineWidgets",
    "Qt6WebChannel",
    "Qt6Network",
    "Qt6DBus",
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

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/qt/bridge.h");
    println!("cargo:rerun-if-changed=src/qt/qt_shell.h");
    println!("cargo:rerun-if-changed=src/qt/qt_shell.cpp");
    println!("cargo:rerun-if-env-changed=QT_MOC");
    for variable in CMAKE_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={variable}");
    }

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

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    let layer_shell_qt = discover_layer_shell_qt(&out_dir);
    qt_include_paths.extend(layer_shell_qt.include_paths);
    qt_include_paths.sort();
    qt_include_paths.dedup();

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

    let mut build = cxx_build::bridge("src/ffi.rs");
    build
        .file("src/qt/qt_shell.cpp")
        .file(&moc_output)
        .include("src/qt")
        .std("c++17")
        .flag_if_supported("-fPIC");
    for include_path in &qt_include_paths {
        build.include(include_path);
    }
    build.compile("vosulay-qt-shell");

    println!(
        "cargo:rustc-link-arg={}",
        layer_shell_qt.library.to_string_lossy()
    );
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
