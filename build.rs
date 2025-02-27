#[cfg(feature = "bundled")]
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

// Use pkg-config to configure the build for a certain hwloc release
fn use_pkgconfig(required_version: &str, first_unsupported_version: &str) -> pkg_config::Library {
    // Run pkg-config
    let lib = pkg_config::Config::new()
        .range_version(required_version..first_unsupported_version)
        .statik(true)
        .probe("hwloc")
        .expect("Could not find a suitable version of hwloc");

    // As it turns-out, pkg-config does not correctly set up the RPATHs for the
    // transitive dependencies of hwloc itself in static builds. Fix that.
    if cfg!(target_family = "unix") {
        for link_path in &lib.link_paths {
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{}",
                link_path
                    .to_str()
                    .expect("Link path is not an UTF-8 string")
            );
        }
    }

    // Forward pkg-config output for futher consumption
    lib
}

// Fetch hwloc from a git release branch, return repo path
#[cfg(feature = "bundled")]
fn fetch_hwloc(parent_path: impl AsRef<Path>, version: &str) -> PathBuf {
    // Determine location of the git repo and its parent directory
    let parent_path = parent_path.as_ref();
    let repo_path = parent_path.join("hwloc");

    // Clone the repo if this is the first time, update it with pull otherwise
    let output = if !repo_path.join("Makefile.am").exists() {
        Command::new("git")
            .args([
                "clone",
                "https://github.com/open-mpi/hwloc",
                "--depth",
                "1",
                "--branch",
                version,
            ])
            .current_dir(parent_path)
            .output()
            .expect("git clone for hwloc failed")
    } else {
        Command::new("git")
            .args(["pull", "--ff-only", "origin", "v2.x"])
            .current_dir(&repo_path)
            .output()
            .expect("git pull for hwloc failed")
    };

    // Make sure the command returned a successful status
    let status = output.status;
    assert!(
        status.success(),
        "git clone/pull for hwloc returned failure status {status}:\n{output:?}"
    );

    // Propagate repo path
    repo_path
}

// Compile hwloc using autotools, return local installation path
#[cfg(all(feature = "bundled", not(windows)))]
fn compile_hwloc_autotools(p: PathBuf) -> PathBuf {
    let mut config = autotools::Config::new(p);
    config.fast_build(true).reconf("-ivf").build()
}

// Compile hwloc using cmake, return local installation path
#[cfg(all(feature = "bundled", windows))]
fn compile_hwloc_cmake(build_path: &Path) -> PathBuf {
    let mut config = cmake::Config::new(build_path);

    // Allow specifying the CMake build profile
    if let Ok(profile) = env::var("HWLOC_BUILD_PROFILE") {
        config.profile(&profile);
    }

    // Allow specifying the build toolchain
    if let Ok(toolchain) = env::var("HWLOC_TOOLCHAIN") {
        config.define("CMAKE_TOOLCHAIN_FILE", &toolchain);
    }

    config.always_configure(false).build()
}

fn main() {
    // Determine the minimal supported hwloc version with current featurees
    let required_version = if cfg!(feature = "hwloc-2_8_0") {
        "2.8.0"
    } else if cfg!(feature = "hwloc-2_5_0") {
        "2.5.0"
    } else if cfg!(feature = "hwloc-2_4_0") {
        "2.4.0"
    } else if cfg!(feature = "hwloc-2_3_0") {
        "2.3.0"
    } else if cfg!(feature = "hwloc-2_2_0") {
        "2.2.0"
    } else if cfg!(feature = "hwloc-2_1_0") {
        "2.1.0"
    } else if cfg!(feature = "hwloc-2_0_4") {
        "2.0.4"
    } else {
        "2.0.0"
    };

    // If asked to build hwloc ourselves...
    #[cfg(feature = "bundled")]
    {
        // Determine which version to fetch and where to fetch it
        let (source_version, first_unsupported_version) = match required_version
            .split('.')
            .next()
            .expect("No major version in required_version")
        {
            "2" => ("v2.x", "3.0.0"),
            other => panic!("Please add support for bundling hwloc v{other}.x"),
        };
        let out_path = env::var("OUT_DIR").expect("No output directory given");

        // Fetch latest supported hwloc from git
        let source_path = fetch_hwloc(out_path, source_version);

        // On Windows, we build using CMake because the autotools build
        // procedure does not work with MSVC, which is often needed on this OS
        #[cfg(target_os = "windows")]
        {
            // Locate CMake support files, make sure they are present
            // (should be the case on any hwloc release since 2.8)
            let cmake_path = source_path.join("contrib").join("windows-cmake");
            assert!(
                cmake_path.join("CMakeLists.txt").exists(),
                "Need hwloc's CMake support to build on Windows (with MSVC)"
            );

            // Build hwloc, configure our own build to use it
            let install_path = compile_hwloc_cmake(cmake_path);
            println!("cargo:rustc-link-lib=static=hwloc");
            println!(
                "cargo:rustc-link-search={}",
                install_path.join("lib").display()
            );
        }

        // On other OSes, we build using autotools and configure using pkg-config
        #[cfg(not(target_os = "windows"))]
        {
            let install_path = compile_hwloc_autotools(source_path);
            env::set_var(
                "PKG_CONFIG_PATH",
                format!("{}", install_path.join("lib").join("pkgconfig").display()),
            );
            use_pkgconfig(required_version, first_unsupported_version);
        }
    }

    // If asked to use system hwloc, we configure it using pkg-config
    #[cfg(not(feature = "bundled"))]
    {
        let first_unsupported_version = match required_version
            .split('.')
            .next()
            .expect("No major version in required_version")
        {
            "2" => "3.0.0",
            other => panic!("Please add support for hwloc v{other}.x"),
        };
        use_pkgconfig(required_version, first_unsupported_version);
    }
}
