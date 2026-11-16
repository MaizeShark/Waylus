// SPDX-License-Identifier: GPL-3.0-or-later
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.waylus"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.waylus"
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Sideloaded over adb only; sign with the debug key for convenience.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
