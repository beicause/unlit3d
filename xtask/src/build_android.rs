//! `cargo xtask build-android`: build the examples' shared libraries, then the
//! APK that packages them.
//!
//! Two tools, split along the language boundary. `cargo ndk` cross-compiles the
//! `*_android` crates, which hold the exported entry points and nothing else,
//! and drops each `.so` into the app's `jniLibs` directory, which is where
//! Gradle looks for native libraries and packages whatever it finds; Gradle then
//! compiles the activities and assembles the APK around them. The libraries are
//! built first because an APK without them is an activity that cannot start.
//!
//! Both tools configure themselves from the environment: `cargo ndk` reads the
//! NDK out of `ANDROID_NDK_HOME` (or the newest one under `ANDROID_HOME/ndk`)
//! and Gradle reads the SDK out of `ANDROID_HOME` (or `android/local.properties`).

use std::path::Path;
use std::process::Command;

use super::BuildAndroidArgs;
use super::step;

/// The crates that build the libraries the activities load, in the order the
/// tab bar lists them. Each is a `cdylib` exporting `android_main`, and each
/// activity names the one it loads in its `android.app.lib_name` manifest
/// entry — the package is the example's name with `_android` appended, while
/// the library keeps the example's own name.
const LIBRARY_CRATES: &[&str] = &["unlit3d_examples_android", "unlit3dx_examples_android"];
/// The ABI the APK carries. Kept in step with `abiFilters` in
/// `android/app/build.gradle.kts`, which is what decides what the APK accepts.
const ABI: &str = "arm64-v8a";
/// The API level the library is compiled and linked against. Kept in step with
/// `minSdk` in `android/app/build.gradle.kts`: linking against a higher one
/// would let the library call into API the APK's own floor does not promise.
const PLATFORM: &str = "26";
/// The Android application's Gradle project, relative to the workspace root.
const ANDROID_DIR: &str = "android";
/// Where the Rust build puts the shared library, relative to the workspace
/// root. This is Gradle's own `jniLibs` source directory, whose subdirectory
/// names are ABI identifiers, so `cargo ndk -o` writes exactly its layout.
const JNI_LIBS_DIR: &str = "android/app/src/main/jniLibs";

/// Build the shared library and the APK that packages it.
pub fn run(args: &BuildAndroidArgs) -> Result<(), String> {
    library(args)?;
    apk(args)
}

/// Cross-compile the libraries and stage them for the APK.
///
/// One `cargo ndk` run builds them all: it passes the packages through to
/// `cargo build` and writes every resulting `.so` into the same `jniLibs`
/// directory, which is what the activities load from at run time.
fn library(args: &BuildAndroidArgs) -> Result<(), String> {
    let profile: &[&str] = if args.release { &["--release"] } else { &[] };
    let mut cargo = Command::new("cargo");
    cargo
        .args(["ndk", "-t", ABI, "-P", PLATFORM, "-o", JNI_LIBS_DIR])
        .arg("build")
        .args(LIBRARY_CRATES.iter().flat_map(|krate| ["-p", krate]))
        .arg("--lib")
        .args(profile);
    step::run(&mut cargo, "the Android build")
}

/// Assemble the APK, which packages whatever `library` staged.
fn apk(args: &BuildAndroidArgs) -> Result<(), String> {
    let task = if args.release {
        ":app:assembleRelease"
    } else {
        ":app:assembleDebug"
    };
    let mut gradle = Command::new("./gradlew");
    gradle.current_dir(Path::new(ANDROID_DIR)).arg(task);
    step::run(&mut gradle, "the Gradle build")?;

    let apk = apk_path(args);
    if apk.is_file() {
        println!("built {}", apk.display());
        Ok(())
    } else {
        Err(format!(
            "the Gradle build reported success but {} is missing",
            apk.display()
        ))
    }
}

/// The APK the selected build type produces.
///
/// Gradle puts it under `build/outputs/apk` on its own, so the path is the
/// build type's name rather than anything this task asks for. A release APK
/// is `-unsigned` because the project ships no signing configuration.
fn apk_path(args: &BuildAndroidArgs) -> std::path::PathBuf {
    let (build_type, file) = if args.release {
        ("release", "app-release-unsigned.apk")
    } else {
        ("debug", "app-debug.apk")
    };
    Path::new(ANDROID_DIR)
        .join("app/build/outputs/apk")
        .join(build_type)
        .join(file)
}
