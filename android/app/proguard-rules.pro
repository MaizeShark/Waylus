# SPDX-License-Identifier: GPL-3.0-or-later
# R8 rules for the release build.

# BouncyCastle: only the EC point arithmetic and the NIST curve table are used (Spake2.kt),
# directly, not through the JCA provider. Keep the curve classes whole, so curve lookup by
# name works however BouncyCastle resolves it internally.
-keep class org.bouncycastle.asn1.nist.** { *; }
-keep class org.bouncycastle.asn1.sec.** { *; }
-keep class org.bouncycastle.asn1.x9.** { *; }
-keep class org.bouncycastle.math.ec.** { *; }
-dontwarn org.bouncycastle.**
-dontwarn javax.naming.**

# Readable stack traces in logcat.
-keepattributes SourceFile,LineNumberTable

# Readable class names in the app's own log lines (exceptions are logged with toString()).
-keepnames class dev.waylus.** { *; }
