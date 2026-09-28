# Solos

A personal AI agent for iOS and Android that runs on your own phone: it talks
to the models you choose (OpenAI-compatible endpoints, Anthropic, Gemini) and
does real work in a Linux sandbox that lives inside the app.

Everything that is not UI (the agent, tools, storage, providers) is one Rust
core shared by both platforms; each platform adds its UI and device services.

> Status: early development. The iOS app is being built; Android has not
> started. See `docs/requirements.md` for scope and `docs/architecture.md`
> for the design.

[简体中文](README.zh-Hans.md)

## Building (iOS)

Requirements: macOS with Xcode, Rust (`rustup target add aarch64-apple-ios aarch64-apple-ios-sim
aarch64-unknown-linux-musl`), and `brew install meson ninja lld libarchive xcodegen`.

```sh
git clone --recurse-submodules <repo>
cd solos
cp ios/Config/Local.xcconfig.example ios/Config/Local.xcconfig   # your team ID and bundle prefix
./deps/build_rootfs.sh          # Alpine rootfs, pinned and checksummed
cd ios && ./build-core.sh       # iSH kernel + Rust core + Swift bindings
xcodegen generate && open Solos.xcodeproj
```

Core tests: `cd core && cargo test`. App tests: the `Solos` scheme's test action.

## Licence

GPL-3.0, with an additional permission for App Store distribution — see
`LICENSE` and `LICENSE.APPSTORE`. The sandbox is [iSH](https://github.com/ish-app/ish),
via an arm64 port of it (`deps/ish`).
