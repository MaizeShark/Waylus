# Releasing

`.github/workflows/release.yml` runs on tags `v*`: it checks the version, builds the Linux host, the Windows host and the app (the jobs of `ci.yml`), and creates a **draft** release with

| Asset | Contents |
|---|---|
| `waylus-<version>-windows-x64.zip` | `waylus.exe`, the FFmpeg DLLs it loads, `waylus.apk`, `README.txt`, `LICENSE.txt`, `licenses/` |
| `waylus-<version>.apk` | the app on its own |
| `waylus-<version>-linux-x86_64-fedora45.tar.gz` | `waylus` built on Fedora 45, `waylus.apk`, `README.txt`, `LICENSE`, `licenses/` |
| `SHA256SUMS.txt` | checksums of the above |

## Checklist

1. **Bump the versions**, both to the same `X.Y.Z`: `version` in the workspace `Cargo.toml` (then `cargo build` to update `Cargo.lock`), and `versionName` in `android/app/build.gradle.kts` (and increase `versionCode` there). Commit and push; wait for CI to pass.
2. **Tag:** `git tag vX.Y.Z && git push origin vX.Y.Z`. The workflow fails if the tag, `Cargo.toml` and `versionName` disagree. A tag with a `-` (`v1.0.0-rc.1`) becomes a pre-release.
3. **Wait** for the *Release* workflow (about 15 minutes) and the draft on the releases page.
4. **Check the draft:** the release notes; the *Android app* job's log shows the signing certificate's SHA-256, which must be the same for every release; ideally unpack the zip on a Windows machine without a development setup and run `waylus --version` and a session.
5. **Publish** the draft.

To try the workflow without releasing, tag a throwaway commit whose versions are bumped to a pre-release such as `0.0.0-ci-test`, push the tag, and afterwards delete the draft and the tag (`gh release delete v0.0.0-ci-test --cleanup-tag`).

## Signing key for the app

Android updates an app only with an APK signed by the same key; otherwise `adb install` fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE` and the app must be uninstalled, which also deletes its Wi-Fi pairing. Release APKs are therefore signed with one release key, kept in the repository's secrets. Without it, `./gradlew assembleRelease` signs with the local debug key, so building works for anyone; the release workflow refuses to build a non-pre-release tag without it.

Create the key once, on your own machine, and keep a backup of the file and the password somewhere safe (a lost key means every user has to uninstall once more):

```sh
keytool -genkeypair -v -keystore waylus-release.jks -storetype PKCS12 \
  -alias waylus -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Waylus"
```

keytool asks for a password. PKCS12 uses it for the store and the key alike. Then add four repository secrets (*Settings → Secrets and variables → Actions*, or with `gh`, which prompts for the value without echoing it):

| Secret | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | `base64 -w0 waylus-release.jks \| gh secret set ANDROID_KEYSTORE_BASE64` |
| `ANDROID_KEYSTORE_PASSWORD` | the password: `gh secret set ANDROID_KEYSTORE_PASSWORD` |
| `ANDROID_KEY_ALIAS` | `waylus`: `gh secret set ANDROID_KEY_ALIAS --body waylus` |
| `ANDROID_KEY_PASSWORD` | the same password: `gh secret set ANDROID_KEY_PASSWORD` |

Don't commit the keystore. To sign a local build with it, set the variables that the workflow sets:

```sh
WAYLUS_KEYSTORE=/path/to/waylus-release.jks WAYLUS_KEYSTORE_PASSWORD=… WAYLUS_KEY_ALIAS=waylus WAYLUS_KEY_PASSWORD=… \
  ./gradlew assembleRelease
```

Anyone switching between a self-built APK (debug key) and a release APK hits the signature mismatch once; the host's error message says to uninstall the app and pair again.

## Notes

- **FFmpeg for Windows** is pinned in `.github/ffmpeg-windows.env` (a BtbN month-end build, which BtbN keeps, checked by SHA-256). To update it, take another month-end build's `win64-gpl-shared` asset and its hash from the release page. `licenses/FFmpeg-SOURCE.txt` in the zip is filled from this file.
- **The Windows exe** is linked with the static C runtime (`+crt-static`), so it needs no Visual C++ redistributable. `.github/scripts/windows-dist.ps1` copies exactly the FFmpeg DLLs that `waylus.exe` loads, fails on imports that a clean Windows lacks, and runs `waylus.exe --version` with only its folder and the Windows folders on `PATH`.
- **Licences:** `.github/scripts/third-party-licenses.py` collects the licences of the Rust crates linked into each binary; the app's libraries are listed in `packaging/android-app-notices.txt` (update it when adding a dependency to the app).
- **Forgejo** falls back to `.github/workflows/` when there is no `.forgejo/workflows/`, so the mirror queues `ci.yml` on every push too; without a registered runner those runs stay *Waiting*. With a Docker runner labelled `ubuntu-latest`, the Linux and Android jobs may work as they are (untested; the Fedora job installs `nodejs` for runners like Forgejo's, and `upload-artifact` v7 is the likeliest step to fail); `windows-latest` needs a Windows runner, and `release.yml` would not work there, since it creates the release with `gh` against GitHub's API.
