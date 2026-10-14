// AGP compiles the Kotlin sources itself, so this project applies no Kotlin
// plugin; the language level follows this AGP version's default. The native
// libraries are not compiled here either — the Rust build writes each into
// `src/main/jniLibs`, which Gradle packages as it is, so that file's name and
// the ABI directory are the whole contract between the two builds.
plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "org.unlit3d.example"
    compileSdk = 37

    // The NDK Gradle strips the Rust libraries with. Pin it to the one
    // `cargo ndk` builds with, so the tool that strips a library always comes
    // from the same toolchain that produced it; AGP's own default would be a
    // different release that this project does not otherwise install.
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "org.unlit3d.example"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "1.0"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
}

dependencies {
    // `appcompat` is what the activities' theme comes from and `core` what the
    // system-UI calls come from; `games-activity` is the activity class itself
    // and `material` the tab bar that switches between the examples.
    implementation(libs.appcompat)
    implementation(libs.core)
    implementation(libs.games.activity)
    implementation(libs.material)
}
