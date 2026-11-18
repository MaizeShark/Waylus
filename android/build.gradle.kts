// SPDX-License-Identifier: GPL-3.0-or-later
plugins {
    id("com.android.application") version "9.5.1" apply false
    // AGP 9 compiles Kotlin itself; Compose only needs the compiler plugin, whose
    // version must match the Kotlin compiler on the build classpath.
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
}
