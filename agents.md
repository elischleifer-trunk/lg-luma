# Luma agent context

## Project

Luma is a native Rust/SDL2/OpenGL ES 2 launcher for rooted LG webOS TVs. Rust source is in
`src/`, original assets/translations in `assets/`, shared vector geometry in `vector-geometry/`,
the host SVG compiler in `tools/vector-assets/`, packaging/tooling in `tools/`, and desktop smoke
scripts in `tests/`. There is no React/JSX/browser application. Read `development.md` for details.

Use pnpm, Oxlint and Oxfmt for build tools; Cargo/rustfmt/Clippy for Rust. `nix develop` supplies
pinned Rust 1.99.1, ARMv7 std, Node 24, pnpm 10.34.5, SDL2, woff2 and Python/fontTools. The webOS
SDK is separate: `pnpm sdk`, then set `WEBOS_NDK` to `.build/toolchain/arm-webos-linux-gnueabi_sdk-buildroot`.

```sh
pnpm install --frozen-lockfile
pnpm format:check
pnpm check
pnpm test
pnpm assets
pnpm test:smoke
pnpm run package
```

When only Markdown (`.md`) files are updated, do not run checks, including formatting, lint,
builds or tests.

`pnpm check` does not execute tests. `pnpm test` is workspace unit tests; smoke scripts render
desktop OpenGL and mock weather HTTP. Linux can use `xvfb-run -a pnpm test:smoke`. Tests must not
contact real weather APIs or a TV. Respect any user instruction to use manual TV verification
instead of running automated tests. Builds/lint may still be run. Do not claim hardware behavior
from desktop checks. Home Assistant compatibility uses API levels, not release versions: raise
`API_VERSION` or `MIN_INTEGRATION` in `src/home_assistant.rs`, or `INTEGRATION_API` or
`MIN_TV_API` in `custom_components/luma/const.py`, only when the other side cannot work without
the change. A raised minimum makes Home Assistant Repairs (and Luma's Settings) ask the owner to
update. Leave version bumps to the manual Release workflow,
which releases the next minor version by default or a major version when chosen. Its
`pnpm version:bump` command synchronizes `package.json`, `appinfo.json`, the root `Cargo.toml`,
the `luma` entry in `Cargo.lock`, README IPK examples and the Home Assistant integration's
`custom_components/luma/manifest.json`. Supporting Rust crates retain
independent versions. `pnpm version:check` checks consistency without writing files. PRs need no
changelog or version files: release notes are generated from merged pull requests.

## Build and identity

`com.ecklf.luma` is the native app's ID and `luma` its executable. Root `appinfo.json` is the
package metadata source. It installs separately from the former web app `com.luma.launcher` and
prototypes `com.luma.launcher.native` and `com.luma.native`, all excluded from app inventory. Preferences are local JSON
per installation; browser local storage and preferences from other IDs are not migrated. Unknown
keys in `preferences.json` and `home-assistant.json` (from a newer build) are kept on save; when
removing a preference field, add its key to `RETIRED_SETTINGS` in `src/model.rs` so it is dropped.

ARM builds must use the NDK's gnueabi soft-float calling convention and explicit sysroot, with
isolated pkg-config. Preserve the Cortex-A9/VFP Rust flags and `getauxval` compatibility.
The SDK's SDL has dummy video: use checksum-pinned SDL-webOS with standard SDL ABI and a real
webOS/Wayland backend. Do not use rust-sdl2's generic bundled SDL or ship host libraries. Native
packages use `Architecture: arm`, preserve executable mode and include licenses/checksums/feed.

## Runtime constraints

Log diagnostics with `telemetry::log_info!`/`log_warn!`/`log_error!(area, …)`, not `eprintln!`:
they reach paired Home Assistant servers as well as stderr (see `src/telemetry.rs`).
Keep input/rendering on the SDL thread and TV/HTTP work off it. Luna requests originate from the
native process, not `luna-send-pub -a` impersonation. Inventory failure must never substitute demo
data on TV. Explicit `--test-extra-apps` is a labeled manual preview applied only after successful
inventory reads, with separate preferences and no placeholder launch requests.

Pointer hover/down must not move a clicked control. Home favorites fill a six-column grid
that wraps onto further rows; launching a favorite moves it to the front (most recently used first). Apps Up/Down follows six-column grid
indices, including offscreen rows, so headers cannot steal mid-grid navigation. Back restores
setting subpage parents and supports SDL-webOS Back. OK finishes move mode without launching.

Vector icon/logo assets are standalone SVGs compiled to `.lvec` at build time. Font outlines
are tessellated only as needed; text runs use bounded GPU caches, lazy CJK and no glyph bitmap
atlas. Text AA prefers MSAA of intact outlines, with a narrow fringe fallback; keep physical
origin/baseline snapping and logical 1920×1080 layout. `--resolution 4k` is experimental, not a
shipping-default claim. Profiles count presentations; ordinary `--frames` is event-loop iterations,
each a fixed 1/60 s of animation time.

Weather uses ureq/rustls, fixed Open-Meteo origins, bounded replies/timeouts, no proxy/redirects,
separate workers/cache and generation IDs. Cache Celsius and convert units without replacing it.
Only configured/enabled/visible Home starts forecasts. Shared translations are bundled locally.
Match the current native design without on-screen weather credits; attribution remains in docs.

## Assets

When adding, removing or renaming a UI language, update `Language::ALL`/picker ordering in
`src/i18n.rs`, bundled translations and required font coverage, `assets/homebrew-description.html`,
and language lists in `development.md`/`readme.md` in the same change. Keep language choices sorted
by displayed name and selection indices consistent. Update translation smoke navigation when ordering
changes. The Homebrew description must name every supported language; keep the
`homebrew_description_lists_every_supported_language` regression test exhaustive so a new enum
variant cannot silently leave release-facing metadata outdated. Do not edit generated feed files.
When adding or changing user-facing features, review `assets/homebrew-description.html` as well as
the README/development docs so the Homebrew listing reflects major capabilities (including uploaded
backgrounds and QR-managed webhook shortcuts), not just languages. Avoid claims for unverified TV
behavior or features not yet implemented.

SVG sources and fonts retain licenses in `assets/`. TV app artwork is resolved at runtime from
inventory/installed manifests by `src/app_artwork.rs`, with a native Rust `luma-helper` elevated
reader for jailed installations. Reads/decoding stay on its worker; GPU uploads stay on SDL.
Refresh invalidates cached results. No collected icon map or card-colour JSON is used or shipped.
Desktop previews use curated vectors and generic/category fallbacks. Do not add dumped TV images
or per-TV icon maps to the repository.
Do not replace vector logos with PNG exports.
Curated vectors use the exact app-ID allowlist in `src/branding.rs`, never title/substring matching.
Every logo in `assets/logos/` must have a desktop demo app. `pnpm test:brands` renders the full
gallery to `.build/brands-gallery.png`; source restoration via `pnpm assets:brands` is optional.

## CI and releases

`.github/workflows/ci.yml` is native-only: format/lint/workspace tests, asset generation and
mocked desktop rendering, SDK setup, ARM cross-build, actual IPK verification/checksum/feed,
and artifacts. It validates only; `.github/workflows/release.yml` calls it and publishes.
`.github/workflows/release.yml` (manual dispatch on `main`) is the only publisher: it bumps the
version (minor by default, or major) on a `release/vX.Y.Z` branch, validates it with `ci.yml`, fast-forwards `main`, creates the
GitHub release and mirrors it to Cloudflare R2. PRs and merges never publish. See `development.md#releases` for setup and retries.
Only create commits or PRs when explicitly requested. For PR descriptions follow the global
`commands/pr.md` format instructions.
