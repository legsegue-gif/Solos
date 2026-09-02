# Solos

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/Platforms-iOS%20%7C%20Android-lightgrey.svg)](#beta-programme)

**Your private, on-device AI agent.**

Solos brings leading models — Claude, GPT, Gemini and more — into a native
mobile experience, and gives them a real computer to work with: a full Linux
shell running on your device, browser automation, extensible skills, persistent
memory, and deep system integration.

It is free, and fully open source.

<a href="https://github.com/legsegue-gif/Solos/releases">
  <img alt="Get the APK on GitHub" height="48" src="assets/badge-android.svg" />
</a>

---

## What it does

| | |
|---|---|
| **Bring your own model** | Claude, GPT, Gemini and other providers, via your own API keys or account sign-in. |
| **A real Linux shell** | A sandboxed Alpine Linux environment runs on-device — the agent can install packages, run scripts, and work with real files. |
| **Device integration** | Health, Calendar, Reminders, Contacts, HomeKit, Bluetooth, Clipboard, Media, Alarms and more, exposed to the agent as tools. |
| **Browser automation** | The agent can browse and interact with the web on your behalf. |
| **Skills & memory** | Extensible skills plus persistent memory across sessions. |
| **Workspaces** | Organise work into separate contexts, addressable via `solos://workspace/`. |
| **Native offloads** | Heavy or platform-specific work is handed to native code instead of the sandbox. |

---

## What you can do with Solos

A few things people actually use it for:

- **Photograph a meal, log the nutrition** — Solos identifies the dishes, estimates
  calories and macros, and writes them to Apple Health.
- **Wake up to your timeline** — Shortcuts triggers Solos to fetch your X timeline,
  summarise it, synthesise speech, and play it as your alarm.
- **Turn group chatter into tasks** — pull messages from a Telegram group, extract
  bugs and action items, deduplicate them, and file them into Apple Reminders.
- **Mount your Obsidian vault** — research, clean up and write Markdown notes back
  into the vault as a normal workspace.
- **Share anything into a calendar event** — send a page or message to Solos via the
  iOS Share Sheet and it creates the event, time and place included.

---

## Skills

A **skill** is a folder with a `SKILL.md` file — instructions, and optionally scripts,
references and assets — that the agent loads on demand when a request matches it.
Metadata stays in context for triggering; the body and bundled resources load only
when the skill is actually used.

---

## Beta programme

Solos is not distributed through the App Store. The iOS build is published
as an unsigned IPA on the [releases page](https://github.com/legsegue-gif/Solos/releases)
and installed by self-signing it (a free Apple ID and AltStore, Sideloadly or
Xcode will do). This is deliberate: the GPL grants every recipient the right to
run and redistribute this software without the additional restrictions the App
Store's terms impose.

On Android, the same releases page always carries the latest APK.

---

## Building from source

Solos ships a Linux sandbox inside the app, so the native dependencies (iSH on
iOS, PRoot on Android, FFmpeg, LAME) and the Alpine rootfs are **built from
source** rather than committed as binaries.

**→ See [BUILDING.md](BUILDING.md) for the full first-build guide.**

The short version:

```sh
git clone --recurse-submodules https://github.com/legsegue-gif/Solos.git
cd Solos

# iOS  — order matters: FFmpeg links against LAME
./deps/build_lame.sh && ./deps/build_ffmpeg.sh
./deps/build_ish.sh && ./deps/prepare_alpine_rootfs.sh
open src/ios/Solos.xcodeproj

# Android — needs NDK r28+
./deps/build_proot.sh && ./scripts/prepare_android_sandbox.sh
cd src/android && ./gradlew :app:assembleDebug
```

`BUILDING.md` covers the toolchain requirements per platform, the build-time
customization templates, and a troubleshooting section for the failure modes
you are most likely to hit.

---

## Repository layout

```
src/ios/          iOS app (Swift / SwiftUI) + share, widget and file-provider extensions
src/android/      Android app (Kotlin / Compose) + JNI native code
src/shared/       Assets shared by both platforms
deps/             Native dependency build scripts and vendored sources
docs/specs/       Architecture and interface specifications
scripts/          Rootfs preparation and developer tooling
```

---

## Acknowledgements

Solos stands on a great deal of open-source work. Our thanks to the
maintainers of these projects — the full inventory, with versions and license
terms, is in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

**The sandbox** — the heart of the product:

- **[iSH](https://github.com/ish-app/ish)** (GPLv3) — Linux usermode emulation on
  iOS. We run [an ARM64 fork](https://github.com/legsegue-gif/solos-ish).
- **[PRoot](https://github.com/termux/proot)** (GPLv2) — user-space chroot for the
  Android sandbox, via [our fork](https://github.com/legsegue-gif/solos-proot);
  **[talloc](https://talloc.samba.org)** (LGPLv3+) underpins it.
- **[Alpine Linux](https://alpinelinux.org)** — the minirootfs the sandbox boots.

**Media & text** — [FFmpeg](https://ffmpeg.org) (LGPL-2.1+),
[LAME](https://lame.sourceforge.io) (LGPL), [cppjieba](https://github.com/yanyiwu/cppjieba) (MIT),
[KaTeX](https://katex.org) (MIT).

**iOS** — [SwiftAnthropic](https://github.com/jamesrochabrun/SwiftAnthropic),
[SwiftMath](https://github.com/mgriebling/SwiftMath),
[RealTimeCutVADLibrary](https://github.com/helloooideeeeea/RealTimeCutVADLibrary) (all MIT),
[swift-cmark](https://github.com/swiftlang/swift-cmark) (BSD-2-Clause), and the
Apple / Swift Server Workgroup packages (Apache-2.0).

**Android** — [AndroidX & Jetpack Compose](https://developer.android.com/jetpack),
[OkHttp](https://square.github.io/okhttp/), [Coil](https://coil-kt.github.io/coil/),
[kotlinx](https://github.com/Kotlin) serialization & coroutines,
[multiplatform-markdown-renderer](https://github.com/mikepenz/multiplatform-markdown-renderer),
[Reorderable](https://github.com/Calvin-LL/Reorderable), [ACRA](https://github.com/ACRA/acra)
(all Apache-2.0), and [Shizuku](https://github.com/RikkaApps/Shizuku-API) (MIT).

---

## License

Solos is licensed under the **[GNU General Public License v3.0](LICENSE)**.

The app links GPL-licensed components — [iSH](https://github.com/legsegue-gif/solos-ish)
(GPLv3) and [PRoot](https://github.com/legsegue-gif/solos-proot) (GPLv2) — so the combined
work is distributed under GPLv3. Bundled third-party licenses are listed in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

---

## Community

- **Issues**: Bug reports, feature requests and discussion via
  [GitHub Issues](https://github.com/legsegue-gif/Solos/issues)

This repository is a mirror of a private development tree, so it **does not
accept pull requests** — there is nowhere for them to land. Issues are the way
to shape the product. See [CONTRIBUTING.md](CONTRIBUTING.md).
