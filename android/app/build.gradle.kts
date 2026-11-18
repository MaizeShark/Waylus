// SPDX-License-Identifier: GPL-3.0-or-later
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    // The Kotlin package stays dev.waylus; only the installed app id changed.
    namespace = "dev.waylus"
    compileSdk = 37

    defaultConfig {
        applicationId = "de.hut9.waylus"
        minSdk = 30
        targetSdk = 35
        versionCode = 2
        versionName = "0.2.0"
    }

    buildTypes {
        release {
            // R8: drops most of Compose, Material and BouncyCastle that the app doesn't use.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Sideloaded over adb only; sign with the debug key for convenience.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures {
        buildConfig = true
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    // UI around the video (idle screen, pairing, settings, stats display). The video itself
    // is a plain SurfaceView. Stable BOM only (ui/Theme.kt explains why not Material 3 Expressive yet).
    val bom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(bom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-splashscreen:1.2.0")
    // QR code of the project URL (ui/Qr.kt); Apache-2.0, GPL-3.0 compatible.
    implementation("com.google.zxing:core:3.5.4")
    // P-256 point arithmetic for SPAKE2 pairing (Spake2.kt); MIT licence.
    implementation("org.bouncycastle:bcprov-jdk18on:1.78.1")
    testImplementation("junit:junit:4.13.2")
}
