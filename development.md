# Luma development

## Layout

```text
Cargo.toml / Cargo.lock       Rust workspace and pinned dependencies
src/                         Rust launcher, platform adapter and GLES2 shaders
assets/                      Editable SVGs, fonts, wallpapers, translations and artwork metadata
vector-geometry/             Shared fill tessellation and vector boundary coverage
tools/vector-assets/         Host-only SVG compiler
tools/                       Asset preparation, SDK/runtime setup, packaging and release feed
tests/                       Desktop rendering and mocked HTTP smoke scripts
tv/autostart.sh              Optional cold-boot launch hook
tv/home.sh / luma-helper         Optional root-only native Home integration
dist/                        Generated ARM IPK, checksum and Homebrew feed
.build/                      Generated assets, SDK/runtime caches and rendering diagnostics
```

The launcher is rendered natively, without React, JSX, Vite or a browser runtime. Node/pnpm
remain build/deployment tools for Sharp, the webOS CLI and formatting/linting. The installed app
is the Rust executable `luma`, with app ID `com.ecklf.luma`. The companion pages in
`assets/wallpaper-upload/` and `assets/tile-editor/` are plain HTML/CSS/JavaScript, embedded in the
native executable and served temporarily to the owner's phone or computer; they do not render the
TV interface.

## Toolchain and commands

```sh
nix develop
pnpm install --frozen-lockfile
pnpm assets
pnpm dev
```

The flake supplies Rust 1.99.1, Cargo/rustfmt/Clippy, Node.js 24, pnpm 10.34.5, SDL2, pkg-config,
woff2, Python/fontTools, CMake and Git. Linux includes Mesa and Xvfb. Supported Nix hosts are
Apple Silicon macOS and ARM64/x86-64 Linux. Without Nix, supply those tools and Rust 1.88+,
including `armv7-unknown-linux-gnueabi` for TV builds. No browser installation is needed.

| Command                   | Purpose                                                    |
| ------------------------- | ---------------------------------------------------------- |
| `pnpm dev [--options]`    | Run the desktop preview                                    |
| `pnpm assets [--4k]`      | Prepare fonts, vector artwork and raster backgrounds       |
| `pnpm screenshots`        | Capture native Home, Apps, General and Inputs into `docs/` |
| `pnpm assets:vector`      | Recompile bundled SVG icon/logo meshes                     |
| `pnpm assets:fonts`       | Restore checksum-pinned Noto font source if needed         |
| `pnpm sdk`                | Download/relocate a pinned host webOS SDK                  |
| `pnpm runtime`            | Prepare checksum-pinned SDL-webOS                          |
| `pnpm build`              | Release ARM cross-build using `WEBOS_NDK`                  |
| `pnpm run package`        | Assets, cross-build, IPK verification and Homebrew feed    |
| `pnpm deploy --device tv` | Build/install/launch through an ares device profile        |
| `pnpm check`              | Oxlint and workspace Clippy, without executing tests       |
| `pnpm format:check`       | Rust and tooling/document formatting checks                |
| `pnpm test`               | Release-tooling and Rust workspace unit tests              |
| `pnpm test:icons`         | Mocked runtime icon reads and bundled vector checks        |
| `pnpm test:smoke`         | Desktop rendering, mocked weather and localization checks  |
| `pnpm version:check`      | Check synchronized app release versions                    |

The TV target is ARMv7 **soft-float calling convention**, `gnueabi`, not `gnueabihf`. Codegen
uses the NDK's Cortex-A9/VFP baseline with NEON disabled for Rust code. Hardware floating point
is enabled without changing the calling convention; the pinned compiler emits unstable-feature
warnings for those target switches. `src/webos-compat.rs` implements `getauxval` for the SDK's
older libc by reading real kernel auxiliary values, used by Rust startup and TLS CPU detection.

The NDK's SDL is a link-time stub with Wayland disabled. `tools/webos-runtime.mjs` instead pins
SDL-webOS `release-2.30.12-webos.5`, standard SDL ABI, with archive/library SHA-256 verification.
It is linked and packaged with runtime name `libSDL2-2.0.so.0`, loaded from `$ORIGIN/lib`.
Desktop pkg-config paths are isolated from the ARM build. `WEBOS_NATIVE_LIBS` can point to
explicitly selected ARM webOS shared libraries; SDL overrides must have a real webOS/Wayland
backend. Avoid copying the whole SDK library directory or the TV's graphics drivers/libc.

## Controls and navigation

### Documentation screenshots

`pnpm screenshots` generates `docs/home.png`, `docs/apps.png`, `docs/settings.png` and
`docs/inputs.png`, linked by the README. It builds the desktop executable without the TV feature,
uses isolated temporary preferences, sample inventory and a fixed date/time, and captures actual
1920×1080 OpenGL framebuffers. Weather is unconfigured, so no real weather APIs are contacted.
It does not execute the test suites. A desktop display is required; on headless Linux use
`xvfb-run -a pnpm screenshots`. These images document the native design, not TV hardware behavior.

### Input

| Input                         | Action                                                        |
| ----------------------------- | ------------------------------------------------------------- |
| Arrows                        | Move focus, revealing remote-focused content                  |
| OK / Enter                    | Activate on release                                           |
| Hold OK for 600 ms            | Reorder Home favorite or open app options                     |
| OK or Back in move mode       | Finish reordering without launching                           |
| Escape / Back / Backspace     | Parent navigation, overlay dismissal or Recent apps from Home |
| Hold Back / Escape for 600 ms | Open Recent apps while Luma is focused                        |
| Menu / M / right-click        | App or background options side popup                          |
| Pointer / wheel               | Hover, click, hold and scrolling                              |
| F5                            | Refresh inventory                                             |

Backspace deletes while editing. Native Back uses SDL's webOS-specific keycode. Back dismisses
the keyboard first; Weather Location then returns to its parent Location row, HDMI choices to
that HDMI row, and the Background gallery to Background in the sidebar. Back from the sidebar
focuses the Settings tab, then returns Home. The header holds Home, Apps and Settings, and at
the right margin a search button (`Target::LgSearch`) that launches LG's own search app. It is
shown only when `listApps` reports one: `platform::lg_search_app` takes the first known ID
(`com.webos.app.search`, `…searchapp`, `…lgsearch`, `…searchviewer`), else any LG system app
with "search" in its ID, else `com.webos.app.voice`; the inventory log names the choice and
every search/voice app seen. Only Right from the Settings tab focuses it (geometric moves skip
it); Left and Back return to the current tab, Down to the page's content.
**General** holds a single update row: the installed version and when it was last checked
(`State::update_checked`), with **Check now** (or **Install**) as a button on the right.
**System** lists **TV settings** (`Target::TvSettings`, which launches `com.palm.app.settings`),
**App names**, **Language**, **Reduced motion**, **Home Assistant**, **Text rendering**,
**Refresh apps & inputs** and the vector power-icon **Quit app** action. **Home Assistant** opens
a subpage with the on/off toggle and, while on, **Add to Home Assistant**, which reopens the HACS
QR code.
On Apps, OK or a normal pointer click on an app opens its side popup, with **Open** first and focused by
default, followed by favorite, hide/show and **Delete app** actions. A second OK launches the app.
Home favorites, Inputs and Recent apps launch directly. Hold OK on Home still reorders favorites.
Background cards open the same side popup with **Use background** first and **Delete background**
only for inactive backgrounds. Deletion requires confirmation with **Cancel** focused by default.
Uploaded images are deleted on the wallpaper worker; bundled assets remain in the package, with
their gallery removal persisted in preferences. The active background is protected in both the
UI and deletion worker. Back restores the gallery card by identity even after catalog changes.
LG's Settings and More buttons retain their system menus, with no custom key interception.
Deletion opens a separate confirmation with **Cancel** focused by default. Cancel or Back returns
to the side popup. Once deletion is sent, dismissal does not cancel the platform operation.
The popup has action icons on the far right, without a title, close button or embedded shortcut hints. The
confirmation has no heading and identifies the app in its warning text.
Luma (including former app IDs and the Home identity), Homebrew Channel, test-preview cards and
all apps in the `com.webos.*`, `com.palm.*` and `com.lge.*` namespaces cannot be deleted. This
conservative vendor protection includes Live TV, Web Browser, Settings and potentially removable
LG apps, rather than relying on firmware to reject unsafe removals. The worker calls
`org.webosbrew.hbchannel.service/exec` once to run the packaged native Rust
`luma-helper uninstall-app` command. It sends one `com.webos.appInstallService/remove` subscription with
`{"id":"APP_ID","subscribe":true}` and waits for status 31, not the initial acceptance reply.
The normal endpoint was manually verified removing Content Store DAZN on webOS 26, whereas
Homebrew's `uninstall` endpoint (`dev/remove`) returned `FAILED_REMOVE` for that app. The helper
rechecks protected IDs, bounds output to 64 KiB/64 replies, terminates luna-send after completion
or a 15-second deadline and has a separate 20-second native transport budget. Luma refreshes inventory after
success. After a failed or timed-out request the worker makes up to three bounded, read-only
inventory checks. An app absent from a complete inventory confirms removal even if its completion
reply was lost. Otherwise the error closes the popup and refreshes inventory without deleting
preferences. Uninstall mutations are never automatically retried. Desktop deletion changes only
in-memory sample inventory. Verify app opening, real deletion, denial and focus restoration
manually on a TV, including Home-takeover mode.
The **Clock** subpage contains **Clock format** (12-hour or 24-hour) and **Time format** (`HH:MM`
or `HH:MM:SS`, defaulting to `HH:MM`). Choosing a format returns to its row in Clock, and Back
from Clock restores its Widgets row (Clock sits above Date). With seconds shown, the header clock redraws once per second
instead of once per minute. Existing saved hour and seconds preferences are retained.
System has an **App names** subpage with independent **Show on Home** and **Show on Apps** toggles.
Home captions default to off and Apps captions default to on, stored independently in `show_home_names`
and `show_app_names`. The legacy `show_names` preference does not override these defaults. Hidden
captions do not reserve layout space.
**Settings → Widgets** groups **Date**, **Greeting**, **Weather**, **Shortcuts**, and **Inputs** as separate subpages.
Weather location and temperature choices return to Weather; Back from a widget returns to its row
in Widgets. Input visibility/naming choices return to Inputs, then its Widgets row.
Existing greeting, weather, shortcut and input preferences are unchanged. Scrollable Settings lists
reserve 64 logical pixels below their last item for both remote focus and wheel scrolling,
with a tighter 48-pixel spacing for Languages and Backgrounds.
**Widgets → Greeting → Show widget** is the first row and hides only the greeting text, preserving the saved name,
date and weather. Weather moves directly below the date when the greeting is hidden, without
moving favorites or inputs. It defaults to enabled for existing and new installations.
**Widgets → Date** contains **Show widget** (enabled by default) and **Date format**. Choose the
existing localized weekday/month/day display (default), `DD.MM.YYYY`, `MM/DD/YYYY`, or `YYYY-MM-DD`.
Choosing a format returns to its Date row; Back from Date restores its Widgets row. Hiding the
date removes its reserved space above the greeting/weather and preserves the chosen format.
Lists render behind the fixed sidebar, navigation and background actions, without viewport clipping
or edge fades. Pointer interaction and remote focus retain their safe content viewports.
The **Greeting** subpage contains **Name** and **Clear name**. Selecting Name opens the text keyboard
(the system keyboard on the TV) and the name is appended to the Home greeting ("Good evening, Ana.").
Long greetings scale down to fit their existing single-line area, centred vertically without
moving the weather summary or favorite cards. Short greetings retain their original size.
The keyboard starts empty; closing it without a name preserves the saved value. Clear name removes
it. Preferences are saved when the keyboard closes, including automatic system-keyboard dismissal.
Back from Greeting restores its Widgets row; App names returns to System. Input name removal
also uses **Clear name**.
Choice checkmarks identify saved values, while the bright row fill identifies only the current focus.

Home favorites fill six columns, then wrap onto further rows that scroll vertically into view.
Left/Right follows saved order, Up/Down follows six-column grid indices (including offscreen rows),
and Up from the first row returns to Home navigation. Reorder mode moves a card by one slot with
Left/Right and by one row with Up/Down. A favorite launched from Home, Apps or recents moves to the
front once it opens and the order is saved (a failed launch changes nothing), so Home lists the most recently used favorites first; apps that are
not favorites are never added. Wheel movement scrolls vertically. Pointer hover/down never moves
the clicked control. Apps Up/Down follows the six-column grid order, including offscreen rows,
and only the first row's Up route reaches the category toolbar. Scrolling Apps content fades into
the existing wallpaper at the header/footer, with one alpha mask for surfaces, shadows, rings,
vector icons/logos and text. The wallpaper/header are unmasked; no blur texture is needed.

TV editing uses the system keyboard through SDL-webOS. Desktop editing uses the custom panel.

HDMI status refreshes every two seconds while the TV's Home or input settings are visible and
focused. Only inputs are read on the existing platform worker; installed apps, artwork and saved
preferences are not reloaded. At most one input read is queued, errors retain the last valid data
and retry after ten seconds, and an unchanged snapshot does not redraw. Holds, reordering, overlays,
loading and hidden/unfocused pages pause polling and defer pending snapshots, keeping pointer
targets stationary through release. A later complete inventory supersedes deferred input data.
The cable state comes from boolean `hdmiPlugIn`, which takes priority over `connected`, with
the existing fallback when unavailable or malformed. Auto shows an input's Home card only while
the cable is present and the TV does not report `hdmiSignalExist: false`, so a console that is
unplugged or switched off takes no slot and the cards after it close the gap. A stale positive
signal cannot override an unplugged cable. Firmware without signal information falls back to the
cable state. **On** keeps the card (with **No signal** in its corner) and **Off** hides it.
**Apps only** keeps the input off the Home strip and instead lists it at the front of the Apps
page's All tab, under the same connected-with-signal test as Auto. It is meant for sources such as
a soundbar that rarely need their own card. There it opens the input on OK and is found by search
under its card title or port; it never appears in category tabs or Hidden.

When Luma runs as Home and another app is in front, the platform worker reads the foreground
app every five seconds through the same direct-or-Homebrew path as other sam/EIM reads. While an
HDMI input is in front it reads every two seconds, and also reads tvpower's power state and then
the inputs only while the TV is `Active` with no pending power change. An input that was seen with
a signal and then reports `hdmiSignalExist: false` or an unplugged cable for five seconds sends
the TV back to Home, so switching a console off returns to Luma instead of a **No signal**
screen. An input opened while its source is still off is left alone, brief drops (mode switches,
HDCP) do not count, and standby disarms the watch so a console following the TV off over CEC
never launches Home. A failed read is logged and retried after thirty seconds. Luma run as a plain
app does not watch inputs, since Home would then be LG's own.

Visible HDMI inputs are the first cards of the Home favorites grid, ordered by port, rather
than a separate row. They launch like apps and cannot be reordered, hidden or deleted. Holding OK on
an input card (or pressing the options key) opens its options: **Open** and **Remove from
favorites**, which switches the input to **Apps only**. On the Apps page an Apps-only input opens
the same options with **Add to favorites**, which returns it to **Auto**. Only **Apps only**
inputs appear on the Apps page. Visibility and names
stay in **Settings → Widgets → Inputs**. Each card shows its port in the corner. A recognized console gives the card its name, brand mark and official
colour: Xbox (#107C10), Nintendo Switch (#E60012) or PlayStation (#003791). Recognition reads what the source reports
about itself in `getAllInputStatus`: the HDMI SPD InfoFrame vendor/product and CEC OSD name fields
first (`spdVendorName`, `spdProductDescription`, `cecDeviceName`, `osdName`, `deviceName`, …),
then the TV's label and other short string values, since field names vary between firmware
versions. Video frames are never sampled. The last console seen on each port is saved in
`input_devices`, so a console keeps its identity when its card returns; on a shared port (an Xbox and a Switch on
one HDMI switch or cable) the card follows whichever console reports itself next. A custom input
name replaces the title, and a name such as "Switch" also picks the colour. Which fields a given
TV firmware fills has not been verified on hardware.

System text events update app/location queries; Done/Back stops editing and restores focus.

### Recent apps

Press Back on Home to open Recent apps, or hold Back (Escape on desktop) for 600 ms from any page.
Short Back navigates on release. Back in an overlay, the system keyboard or
move mode keeps its immediate dismissal behavior; Backspace keeps its editing/navigation role.
Losing window focus cancels an unfinished hold. This is an app-local shortcut while Luma is focused.
SDL-webOS Back (scancode 482) and long-Back Exit (505) are normalized before rust-sdl2 decodes the
event, since the native keymap can leave their keycodes unset and the Rust scancode enum does not
include them. Both Back and Exit access policies are claimed before video initialization. The
native Exit signal opens the switcher without waiting for another timer; repeat/dismissal events
are consumed until release, so the same press cannot dismiss an overlay and then reopen it from Home.

The switcher uses an overlapping landscape-card stack on a dimmed wallpaper, a short, reversible
crossfade to and from the actual parent interface, and a non-bouncy, interruptible depth transition.
The front card is largest; neighboring cards recede in scale and brightness. Cards are painted and
hit-tested back-to-front, so only exposed portions of rear cards can be selected with the pointer.
Reopening during dismissal reverses from the current opacity and keeps the visible cards.
Dismissal restores parent input immediately while the outgoing cards finish fading. The transition
does not move card hit targets; pointer interruption pauses only carousel movement, not the fade.
Reduced motion makes the crossfade, navigation and close transitions immediate.
Pointer hover/down preserves the current card positions and interrupts remote carousel movement.
The backdrop uses 86% opacity so more of the wallpaper shows through. There are no app captions, action buttons,
empty-state subtitle, heading, separate dismiss button, card count, footer instructions or Home
header shortcut. Up immediately lifts the focused card off the top of the screen; after a successful
close, the remaining cards slide into the gap and the next card comes forward. A failed close
returns the card to the stack. Exiting cards have no hit target, and closing the final card finishes
its exit before showing the empty state. Held-Up repeats are ignored to prevent
unintentionally closing more apps as the next card becomes focused. Back dismisses the switcher; Quit app remains an
explicit Settings → System action.

| Input                 | Action                                      |
| --------------------- | ------------------------------------------- |
| Left / Right or wheel | Browse recent cards                         |
| OK on a card          | Launch the app                              |
| Up from a card        | Close that card immediately                 |
| Pointer               | Select a card                               |
| Back                  | Return to the exact previous page and focus |

The platform worker uses the stock-Home operations captured on webOS 26:

| Operation | Luna endpoint                                 | Payload                                                                                            |
| --------- | --------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| History   | `com.webos.service.recent/listRecents`        | `{}`                                                                                               |
| Close one | `com.webos.service.recent/removeRecent`       | `{"appId":"APP_ID"}`                                                                               |
| Launch    | `com.webos.service.applicationmanager/launch` | `{"id":"APP_ID","params":{},"reason":"recent","checkUpdateOnLaunch":true,"autoInstallation":true}` |

Service-name capitalization is significant. These fixed calls go through the native process's
authenticated Homebrew Channel `exec` connection, using bounded root `luna-send` commands and no
application impersonation. Mutations are sent once, checked for `returnValue: true`, then history
is refreshed. They are not automatically retried after an ambiguous timeout. One request is active
per overlay; generation IDs discard late replies after dismissal or reopening. Read/action errors
stay in the overlay with a read-only Retry action and diagnostic details in stderr.

History order is preserved, including input entries, with Luma/Home identities and duplicates
excluded. The native integration still needs manual verification in both normal-tile and
Home-takeover modes, including app/input launching, closing individual apps, short/long Back,
focus restoration and service-denial handling.

Cards share the Home/Apps face renderer: the same exact-ID curated vectors, surfaces, relative logo
proportions, installed icons and generic fallbacks. Recent splash/snapshot artwork is not loaded.
Installed icons keep the existing artwork worker and bounded caches. No TV images or recent history
are bundled or saved into preferences. System and OTNCABI installation roots are supported by the
bounded reader.

Desktop preview uses in-memory sample history and simulated mutations, with no TV calls:

```sh
pnpm dev --script recents,await-recents
```

`hold-back` is also a scripted opening action. Desktop smoke scenarios render normal, close-one,
empty, launch, pointer and Home Back flows; unit tests cover hold timing, cancelled
presses, stale responses, failures, navigation, reduced motion, reversible crossfades and consistent card artwork.

## Custom wallpaper uploads

**Settings → Background → Add a background** opens a native modal with a QR code and a
temporary HTTP URL. On a phone or computer on the same network, choose a JPEG, PNG or WebP,
enter a name, then drag the preview or use the zoom/position sliders to compose a 16:9 crop.
**Add to TV** adds the named image to the gallery without selecting it. Close the TV modal
and choose its thumbnail to use it. The upload page is currently English; the TV modal is localized.

The phone accepts files up to 16 MiB and 32 megapixels, applies image orientation, and exports
only the selected 1920×1080 JPEG crop. The native worker independently verifies the image format,
exact output dimensions, a 12 MiB transport cap and a 32 MiB decoder allocation limit. It strips
source metadata by re-encoding, generates a 480×270 thumbnail, and applies the same base opacity
and readability gradients as bundled wallpapers. Processing and discovery run off the SDL thread;
GPU uploads remain on SDL. Nothing is fetched from external services.

The gallery holds up to 20 uploaded images under `wallpapers/custom-<random-id>/` beside the
selected preferences file. The phone page shows this resolved absolute directory path. Names are
trimmed, control-free and limited to 40 characters; they
never become file paths. Files and metadata are committed in a new directory by atomic rename,
with immutable paths so existing GPU texture caches cannot show an older upload. Root Home
preserves the app storage owner's UID/GID for new directories and files. Preferences retain only
the generated selected ID. Invalid metadata and entries with missing files are skipped; an unavailable selected image
renders the bundled default without destroying the saved selection. These files survive app
updates, and normal/takeover launches using the shared preferences location use the same gallery.
Custom images are not used as LG's launch splash: runtime splash updates are skipped for them,
and takeover boot staging uses a bundled fallback.

The fixed actions are **Upload**, **Gradient**, and **Rotation**, from left to right.
**Gradient** toggles readability shading immediately without moving the controls. Its
`wallpaper_gradient` preference defaults to On for new and existing installations. Bundled assets
include full-resolution `-plain` variants; new uploads retain a metadata-stripped `original.jpg`
alongside the shaded image. Discovery checks for originals on the worker, not the SDL thread.
Older uploads keep their full-resolution shaded image when Off if no unshaded original exists.
The setting does not affect gallery thumbnails or LG's static launch splash.

The fixed **Rotation** button toggles cycling On/Off,
defaulting to Off, without opening a subpage. All available images participate in gallery order;
deleted and pending-deletion entries are excluded. The fixed 15-minute interval uses a persisted
Unix timestamp; manual selection or enabling cycling restarts it, and a clock behind the last
change waits. On process start, rotation advances once after the gallery catalog loads, or after
three seconds if the catalog is delayed. Changes wait until Luma is focused, visible, idle, and outside overlays. Automatic
changes use a 1.6-second fade (instant with reduced motion) and save without refreshing LG's splash.
`splash_background` preserves the manual selection across unrelated saves, package restaging, and
process restarts; old preferences fall back to `background`. Old duration preferences load as On,
while former per-image exclusions are ignored.

The native process binds an ephemeral port on one private/link-local IPv4 interface only while
the upload modal is open and visible, with a fixed ten-minute session lifetime. Closing the modal,
hiding Luma or exiting invalidates the session. On TV, losing focus also closes the modal and
invalidates the session; desktop previews keep it alive when switching to a browser. An unpredictable 128-bit capability
URL, exact Host/Origin checks, a custom upload header, no CORS, and restrictive browser security
headers limit access. The 8-character case-sensitive base-58 token has about 47 bits of entropy;
each session accepts at most 64 local requests and expires after ten minutes. HTTP is **not
encrypted**; use a trusted local network. The URL is not logged.
The branded code follows the small center-logo approach shown in
[Vercel's QR example](https://github.com/vercel/examples/tree/main/container-images/demo/apps/qr):
Luma uses high error correction, light modules on the modal's dark background, and a compact center
mark. QR readers vary in their support for inverted polarity; the URL remains visible as a manual
fallback.
Requests are serialized and bounded by header/body sizes and a deadline, with cancellation checks
while reading and before committing. No user-controlled path is opened by the server. No privileged
helper or permanent system service is installed. Networks with client isolation, IPv6-only access
or an inbound firewall may prevent the phone reaching the server; this needs manual TV verification.

`pnpm test:wallpapers` checks browser crop/name math. Rust wallpaper tests use generated images,
isolated temporary storage and loopback HTTP only. Scripted desktop runs bind the upload server
to loopback rather than the real LAN; `await-upload` waits for the modal's link or error state.
Verify the QR code, phone drag/zoom, upload,
TV selection/restart, normal/takeover access, cancellation and timeout on real hardware; desktop
checks do not establish webOS jail networking permissions or TV/browser behavior.

## Shortcut editing on a phone or computer

**Settings → Widgets → Shortcuts → Configure** opens the same QR modal as the wallpaper
upload, served by its own worker (`src/tile-editor.rs`). Both share `src/companion.rs`: interface
choice, token, QR code, request parsing, Host/Origin checks and security headers are identical,
including the ten-minute lifetime and invalidation on close, focus loss, hiding or exit. A page load
needs about six requests, so a session accepts up to 256 instead of 64. The page is English, like
the upload page.

When the modal opens, the worker takes a copy of the saved tiles and serves `tiles.json` (tiles,
choosable icons with their bundled SVG markup, and the limit). `POST save`, `remove` and `send`
need the session token in `X-Luma-Token` and a JSON body of at most 4 KiB. Saves are validated with
the preference validation rules (`clean_input_name`, `valid_tile_url`, `TILE_ICONS`, `TILE_LIMIT`,
`new_tile_id`).
Creation and editing are available only on this companion page, which uses a flat background.
The TV lists saved shortcuts as selectable description rows matching HDMI settings. Pressing one
opens a popup with **Test shortcut** and **Remove shortcut**; Back returns to the same row.
The System version retains a separate muted, non-interactive surface. Removal clears the
shortcut from preferences, favorites and hidden apps without attempting a platform uninstall.
Each accepted companion change sends the complete list back to the launcher:
new tiles join the end of the Home favorites, removed ones leave favorites and hidden apps, and
preferences are saved. Accepted changes are revisioned and queued before replying to the page;
reopening retains the worker's latest list until the launcher acknowledges every pending revision.
Changes are applied even if the modal closed meanwhile, since the page was already told they were
saved. The TV modal blocks other input, so the two editors never interleave.
`send` makes the request from the TV with the same client as a tile press (five-second timeout, no
redirects or proxy) without saving, and reports the result to the page.

`pnpm test:tiles` checks the page helpers. Rust tests cover validation, the routes, header checks
and session rotation over loopback; `await-tile-editor` waits for the modal's link in scripted runs.
Verify scanning, editing and a test request against a real webhook on TV hardware.

## Home Assistant overlay

**Settings → System → Home Assistant** (`home_assistant`, default Off) starts a persistent
listener on port 5862 (`src/home_assistant.rs`), unlike the short-lived companion sessions. It binds
every interface, because the TV's address can change, and answers only loopback, private and
link-local peers. Requests are small and handled in turn on the service's own thread, with a
three-second deadline and a 32 KiB body limit, reusing `companion::request`.

Pairing: `POST /api/pair` shows a six-digit code as a system toast. In Luma, `Event::PairingCode`
puts it in the setup dialog in place of the QR code (`State::home_assistant_pairing`), opening the
dialog when no other dialog is up and returning focus afterwards; otherwise it is a notice.
`Event::PairingEnded` (expired or locked out) brings the QR code back, and `Event::Paired` shows a
confirmation in the dialog. `POST /api/pair/confirm` trades it for a 256-bit token. Codes expire after two minutes,
five wrong codes cancel the request, new requests are rate-limited, and at most eight servers
pair. `home-assistant.json`, next to the preferences file, stores a stable device ID and SHA-256
digests of tokens with mode 0600. Turning the setting off closes the listener, closes any overlay
and forgets every pairing, but not the camera arrangement: each server's entries move to
`Gallery::parked` (by entity ID) and are restored when the same server pairs again, matched by
the Home Assistant instance ID the integration sends as `server`, or by its name when either side
has none and no other parked server shares that name. Unpairing one server parks its entries the same way; at most 16 are kept. Requests with
an `Origin` header are refused, except the overlay page's
close report, which must come from loopback with the session's random nonce.

The overlay is a separate web app, `overlay/` (`com.ecklf.luma.overlay`), because only a web app's
`appinfo.json` can ask for the transparent `defaultWindowType: overlay` window that keeps the
foreground app running; SDL-webOS cannot set `_WEBOS_WINDOW_TYPE`. `tools/package.mjs` builds it
as its own IPK with Luma's version and stages it in Luma's package as `overlay/overlay.package` (ares-package drops any path
containing `.ipk` from an app; the helper copies it to `/tmp/luma-overlay.ipk` to install it)
with its `appinfo.json`, which `tools/ipk.mjs` verifies. `luma-helper overlay install` (root, in
process for a root Home) installs it via `appInstallService/dev/install` when the installed
version differs. The page is plain script for webOS 4's Chromium 53; `overlay/logic.js` holds the
testable helpers. Luma launches it with launch parameters; a second request relaunches it, which
the page handles via `webOSRelaunch`. Back or its timer closes the page, which reports to Luma.

`pause: true` asks `luma-helper overlay pause` for the first `playing` pipeline from
`com.webos.media/getForegroundAppInfo` and pauses it. Luma resumes that media ID once the page
reports closing, on `DELETE /api/overlay`, or `duration` plus five seconds after launch, whichever
comes first. A replacement overlay keeps the original paused ID. System toasts use
`com.webos.notification/createToast`, falling back to `luma-helper overlay toast`, which sends as
Luma with root `luna-send -a`.

The Home Assistant integration lives in `custom_components/luma` (installable through HACS,
`hacs.json`). It discovers LG TVs through the LG webOS TV integration's SSDP announcement and
keeps only those where `GET /api/info` answers as Luma, suggests hosts and names from existing
`webostv` entries, pairs in its config flow, and starts reauth when the TV answers 401. For a
camera entity it sends Home Assistant's `camera_proxy_stream`/`camera_proxy` URLs signed with the
camera's access token, from the internal URL unless `base_url` is given. The **Show example camera card**
button sends a snapshot-only overlay of `custom_components/luma/example.jpg`, served without auth at
`/api/luma/example.jpg`, so owners can preview the card without a camera.

**Cameras app.** Each loaded config entry also sends `POST /api/cameras` with every available
camera entity (sorted by name, at most 32) and its signed proxy URLs: once at setup and two seconds
after any `camera.*` state change, which includes every access-token rotation, coalescing bursts.
Every minute it also reads `GET /api/status`, whose `cameras` counts only the asking server's list, and resends when it is 0, since
Luma keeps lists in memory only, one per paired server (keyed by token digest, merged in pairing
order; unpairing drops that server's list). While Home Assistant is on, `State::sync_custom_tiles` adds the
built-in `luma.cameras` app (`model::CAMERAS_APP`, the shortcut tiles' `custom` surface, `camera` mark) to the inventory,
so it can be favorited, hidden and searched; `model::is_builtin` keeps it out of uninstall and
inventory-failure checks. Launching it sets `State::show_cameras`; the main loop asks the
service's thread, which answers with `Event::Cameras { open: true }` (or an `Event::Notice` for
a missing pairing or empty list) carrying every camera as `cameras::Camera`, in the owner's saved
order with `hidden` flags and the live-video choice. Changed lists (rotated tokens, unpairing,
turning the setting off) arrive with `open: false`; an open gallery keeps its arrangement on
screen, takes the new URLs and adds new cameras at the end, and an empty list closes it.

The gallery is native (`src/cameras.rs`, drawn by `src/cameras-view.rs`) and opens as the
opaque full-screen `Overlay::Cameras`. `cameras::Layout` keeps 16:9 tiles in one large view, 2×2,
or three across from five cameras, fitting two rows and scrolling the rest. Hold OK → **Rename**
opens the keyboard (`TextField::CameraName`) in the overlay's place, with `Gallery::renaming`
keeping the camera and the gallery's Back target, so the gallery stays drawn and fed behind it
(`State::cameras_visible`); committing restores `Overlay::Cameras`. Names are saved with the
arrangement (`names`, by gallery camera ID) in the Home Assistant store and come back as
`Camera::label`; **Reset name** removes one. Each frame is fetched off the SDL
thread: `State::camera_watches` lists what is on screen (nothing unless the gallery is open and
Luma visible), and `cameras::Service::sync` keeps one worker thread per watched camera, keeping a
worker (and its connected stream) that is watched the same way and only giving it new URLs.
Grid tiles poll snapshots every 2 s (4 s above nine cameras); an enlarged camera reads the
motion-JPEG stream with `cameras::Parts`, which splits parts by Content-Length or JPEG markers
and skips to the newest buffered frame. A stream that fails, ends or sends nothing for 5 s
(ureq's per-read timeout) switches to back-to-back snapshots, at most four a second, and a
**Live** or **Snapshots** badge says which is showing. Frames are decoded (at most two at once)
and shrunk to their drawn size on the worker, then drawn with `Draw::Camera`, which keeps one
GPU texture per camera, re-uploaded only for a newer frame; damage tracking compares frames by
pointer, so a new frame redraws only its tile. The main loop wakes every 16 ms while the gallery
is open.

The enlarged camera can be rewound. Its worker (`Watch::record`) keeps every frame as the camera
sent it, undecoded, for `cameras::REWIND` (two minutes, at most 48 MB) in the service's
`Recording`; it is dropped once no camera is enlarged. A camera Home Assistant's UniFi Protect
integration records arrives with `history_url` (`Camera::history`) and reaches `HISTORY` (24 h)
back. `Playback::Replay { lag, since, speed }` is the position: `lag` behind live at `since`,
moving at `speed` (0 paused, 1 normal, ±2…64 from the media keys, `Gallery::faster`). Left/Right
(`Gallery::skip`) step 10 s, growing while held (`step`: a minute after 1 s, five after 3 s, half
an hour after 6 s; presses within `HOLD_GAP` continue a hold); OK pauses and plays; Up/Down
(`Gallery::jump`) go to 2 s before the previous or next `Mark`. Fast-forward stops at live and
rewind at the oldest moment. Each tick `State::camera_replay` hands the main loop a `Want`: a kept
frame (`Want::Kept`) within the kept span, else Protect's picture (`Want::Recorded`,
`history_url&at=<unix>&width=1280`). One replay thread loads it, newest ask first and not again
for the same frame, as `Update::Replay` under its own texture key; `None` means nothing was
recorded then (**Nothing recorded at this time**). Every `EVENTS_REFRESH` the gallery asks
`history_url&events=1&start=&end=` for the last day's marks (`Update::Marks`), drawn on the bar
by kind. Playing at normal speed in the recordings, once scrubbing has settled for `SETTLE`,
hands over to the overlay app (`Video::Recorded`, `VideoRequest::recorded`), which plays Protect's
MP4 a piece at a time (60 s, then 300 s) from `GET /api/overlay/recorded`: loopback only, checked
against the session's nonce, it redirects to `history_url&start=&end=` with the camera's current
token. Any timeline key there closes it with `key`, `behind` (seconds behind live) and `paused`
in its `closed` report (`cameras::Closed`), and the gallery carries on from that moment with the
key; a video that fails puts the camera in `recorded_failed`, which replays pictures instead.
The live HLS video hands over the same way: Left past what it kept, or Up (back to live when no
mark is earlier), when the camera has history (`VideoRequest::history`). While recorded video
plays, no pictures load behind it; a camera without history paused past its oldest kept frame
keeps the picture it has. Leaving live closes nothing; going live again asks for the
HLS video again.

The integration's `HistoryView` (`/api/luma/history/<entity>`) checks the camera's access token
like `StreamView`, finds the camera's UniFi Protect config entry in the entity registry, and
redirects `at` and `start`/`end` (at most 15 minutes, within 31 days) to the UniFi Protect
integration's snapshot and video views with a path `async_sign_path` signs for a minute as Home
Assistant's content user, passing the camera's entity ID (those views look a camera up by Protect
ID, then entity ID). `events` answers from the Recorder's history of the camera device's Protect
motion and detection binary sensors and doorbell event entity (`event_spans`), told apart by each
entity's own key (its unique ID after the MAC address), never by entity IDs that carry the device
name.

Home Assistant's `camera_proxy_stream` is built from snapshots for most camera integrations
(`Camera.frame_interval` defaults to 0.5 s, so about two frames a second). For real video the
integration adds `hls_url` to every camera with the stream feature while the `stream`
integration is loaded: `/api/luma/stream/<entity>?token=<camera token>` (`StreamView`, which
checks the camera's access tokens like Home Assistant's camera proxy) starts Home Assistant's
HLS stream and answers `{"url": "/api/hls/<stream token>/master_playlist.m3u8"}`. Enlarging a
camera with **Live video** on sets `cameras::Video::Resolving`; `cameras::Service::resolve` asks
that endpoint off the SDL thread, and the answer makes the Home Assistant thread launch the
overlay app with `mode: "video"` (`Api::show_video`); the motion-JPEG worker keeps running
underneath. `luma.dismiss` (`DELETE /api/overlay`) closes only a doorbell card, never a camera video.
The page plays the playlist with the bundled hls.js light build (1.5.20, ES5, Apache-2.0,
`overlay/vendor/`) through Media Source Extensions: it reads Home Assistant's low-latency HLS
parts (about 1 s) instead of waiting for whole segments (about 6 s), so the video starts in a
couple of seconds and stays close to live. The TV's own HLS player is the fallback; it
started after about 12 s on a TV. HLS views allow any origin, so the `file://` page may fetch
them. webOS shows hardware video on a plane behind the page, black until the first frame
decodes, so the page covers it with the camera's snapshots (the `snapshot` parameter,
refreshed back to back) and uncovers it once playback has advanced 0.3 s. hls.js keeps two minutes
behind the playhead and keeps loading while paused or behind (Home Assistant's playlist lists only
the newest few seconds), so Left/Right (and rewind/fast-forward) scrub through what the TV has
buffered and OK (and play/pause) pauses and plays, with the same bar as Luma's; a user pause is not
a stall. Back closes it (OK too, before it plays)
and reports closing, which also leaves the enlarged camera (`Event::VideoClosed`); a stream that
fails, or shows nothing within 15 s, reports `failed`, and that camera stays on the motion-JPEG
stream for the rest of the visit. A doorbell card replaces the video; leaving the camera in Luma
closes it (`Service::close_video`).

A doorbell card can play HLS too. `luma.show_camera` with a camera entity (`mode: video`, the default)
starts the stream in Home Assistant (`card_hls_url`, the same `async_request_stream` as
`StreamView`) and adds the absolute playlist URL as `hls_url`; a camera that cannot stream just
leaves it out. `POST /api/overlay` validates it like the other camera URLs and passes it to the
page as `hls`, still requiring `stream_url` or `snapshot_url` as the card's picture. The page
plays both through `playHls` (the card: native HLS, then hls.js; the Cameras video: hls.js first; 15 s to show a frame; an `ended` video or
one whose `currentTime` stands still for 10 s fails too): the card's muted `<video>` stays
invisible over the motion-JPEG until it plays, then covers it. The motion-JPEG stays connected
underneath, because its camera token may have rotated by the time a long card's video fails, so a
failed video just uncovers it. Home Assistant gives the stream 5 s to start, and any failure there
sends the card without `hls_url`. Older Luma builds ignore `hls_url`.

Holding OK (or a long pointer press, or Menu) on a tile opens **Move** (arrows move it, OK or Back
ends), **Hide camera** and **Camera settings**; the settings button at the top right, reached
with Up from the first row, opens the same settings: **Live video** (off makes enlarged cameras
poll snapshots) and a shown/hidden toggle for every camera. Each change goes to the service's
thread (`Service::arrange`), which stores it in `Store::gallery` by gallery camera ID (the
server's pairing position and entity ID, so two servers' `camera.door` stay separate; unpairing
a server drops its entries and renumbers later servers', and turning the setting off clears it), keeping
entries for every camera the gallery did not report; new cameras follow in Home Assistant's
order, and one arriving while the gallery is open goes after its neighbour in that order. Back ends moving,
closes a sheet or the enlarged camera, then the gallery; Home also closes it.

Rust tests drive the API without sockets through a recording `Tv` (pairing, lockout, validation,
pause/resume, timeouts, persistence) plus one loopback HTTP round trip; helper tests use the
fixture `System`. `pnpm test:overlay` checks the page helpers and window flags. The Home Assistant
tests need `pytest-homeassistant-custom-component` (see `.github/workflows/home-assistant.yml`):

```sh
python -m pip install pytest-homeassistant-custom-component==0.13.316 ruff==0.17.3
ruff check . && ruff format --check .
python -m pytest -p no:cacheprovider tests/home-assistant
```

Turning the setting on opens a dialog (`Overlay::HomeAssistantSetup`) with a QR code for
`HACS_LINK`, a My Home Assistant redirect that opens HACS on the owner's server with this
repository's integration; it shows the listener's address once bound. While the setting is on,
**Add to Home Assistant** (`Setting(9)`) reopens it, so seeing the code again never requires
turning the setting off and forgetting pairings. The QR code uses the companion dialogs'
inverted drawing (`view::qr_code`).

Desktop builds serve the same API and print overlay launches and toasts instead of calling webOS.
Verify overlay stacking, key focus, pausing and the overlay install on TV hardware.

## Splash images

`pnpm assets` renders one 1920×1080 PNG per background into `.build/assets/splash/` from the shaded
wallpapers. `tools/package.mjs` ships them under `splash/` plus `splash.png` (the default background),
which `appinfo.json` names as `splashBackground`, with `bgColor` set to the UI base colour so the
frame between the splash and the first draw is not black. On a normal TV launch and after successful
preference saves, the native platform worker passes the loaded background to the native
`luma-helper sync-splash` command through Homebrew Channel's elevated `exec` service. It atomically updates
the installed `splash.png` and the matching enabled Home stage, skipping identical images. Desktop
and isolated `--preferences` / `--test-extra-apps` runs do not update launch splashes. The first launch
after installation or a package update can still show the packaged default until this sync completes;
later launches use the saved selection. Already-root Home processes use the Rust file operation directly, avoiding a Homebrew/child-process round trip. With Home takeover enabled, the native Home helper
copies the selected background's splash into the staged Home app on every apply, reading the saved
preferences from the TV storage candidates in `storage.rs`. Production installations prefer internal
storage; developer installations prefer developer storage, with existing files taking precedence over
the legacy installation-local preferences. Reapplying an active Home mount also refreshes the splash.
`pnpm test:home` checks splash selection, runtime updates and mounted refreshes with a mocked filesystem
and no TV calls. Verify the system-rendered splash on a real TV by changing Background, quitting Luma,
and relaunching its normal tile; desktop checks cannot establish SAM/compositor image caching behavior.

## Vector rendering

`assets/ui-icons/` holds standalone SVGs exported from the original Lucide/control artwork,
including the supplied PlayStation/Apple TV marks. `assets/logos/` holds original bundled logos.
`tools/vector-assets/` parses SVGs with usvg, resolves stroke outlines and transforms, and uses
the shared lyon tessellator to emit `.lvec` geometry. The format contains positions, edge offsets,
vertex colors and coverage; holes, round strokes and the bundled gradients are preserved.
Gradient colors are adaptively sampled at build time. The TV uploads each mesh lazily once and
draws with a texture-free GLES2 shader. No SVG parser runs on the TV. The shared tessellator also
handles font outlines. Installed app PNG/JPEG/WebP artwork, wallpaper photos, tile-background
images and the webOS installation icon remain raster assets. See [assets.md](assets.md) for licenses.

TV and desktop windows default to 4× MSAA with 2×/single-sample fallbacks and no text
emboldening, restoring the text treatment used in v2.8.0. **Settings → System → Text rendering**
offers **MSAA (default)** and **Analytic**. The `text_rendering` preference (`msaa` or `analytic`)
is saved immediately and applies when Luma next starts because SDL selects MSAA when it creates
the window. Missing or invalid preferences use MSAA. The setting shows a restart reminder.

Analytic mode uses a single-sample window and analytic text/logo edge filters. Text up to 16
physical pixels is emboldened by 0.25 px per edge, fading out by 28 px. The choice is visual:
analytic rendering was preferred on a contributor's webOS 5 TV, while MSAA was preferred in
manual Mali-G52 TV comparisons. `LUMA_MSAA=4` or `0` overrides the saved mode and its default
emboldening. `LUMA_EMBOLDEN` overrides the strength independently. Diagnostics report the
startup `text_rendering` configuration, and `last_presented.multisample_samples` reports the
actual window's samples. Offscreen captures can still be single-sample.

Shape edges use a one-drawable-pixel box filter centred on the edge, and rectangles (and logo
placement) snap to whole drawable pixels. A straight edge is then a single step, and a one-pixel
border is fully opaque. On TVs that scale a 1080p graphics plane up to the panel, half-covered
edge pixels would otherwise become soft two-pixel bands. `LUMA_SNAP=0` keeps logical positions
for comparison.

### Installed app artwork

TV inventory retains each app's `folderPath`, `largeIcon` and `icon`. Visible cards resolve artwork
from that metadata and the installed `appinfo.json`, preferring large icons and falling back to
the regular icon or `icon.png`. Developer, store and system application directories are supported,
including absolute/`file://` paths and relative resource paths. Curated vector brand logos retain
their existing display treatment; other apps use their own installed artwork and an automatically
sampled card colour. Missing, corrupt or unsupported artwork uses the existing generic treatment.

Discovery, bounded file reads, decoding and colour sampling run on a dedicated worker. If the
jailed app cannot read an asset directly, the native process calls Homebrew Channel's elevated
`exec` service with the packaged native `luma-helper read-app-icon` command. It returns bounded
image bytes without writing collected files or changing filesystem permissions. PNG, JPEG and WebP
are supported; runtime SVG parsing is not required. Source files are limited to 512 KiB and
2048×2048 pixels; decoded icons are at most 256×256 with a 16 MiB/256-entry CPU cache. GPU uploads
stay on the SDL thread and share the existing 48 MiB texture cache. Pending/unavailable icons do
not block input, and failures are cached until inventory is refreshed.

Inventory also refreshes at startup and whenever the window becomes visible. Decoded icons are
keyed by app ID, version and icon paths, so those refreshes keep them and retry only previous
failures; an updated app has a new version and is read again. **Refresh apps** (or desktop F5)
invalidates all artwork, including previous failures; request tokens discard stale replies. Check newly installed/updated apps and both normal tile and Home-takeover
launches on a real TV to verify sandbox access. These paths are covered with mocked filesystem
and worker tests, not live-TV calls in automated checks.

Dumped TV artwork, collected icon/colour maps, and their collection tools have been removed.
Desktop previews use curated vector logos or generic/category artwork for other demo apps.
Asset generation clears retired dumped-artwork outputs from older build caches, and packaging
continues rejecting those legacy files. Adding support for an installed TV app's icon does not
require collecting artwork or changing JSON.

### Curated brand cards and the desktop demo

`src/branding.rs` contains the exact app-ID allowlist for 38 curated brand and utility cards. Titles and
partial ID matches do not select vector overrides. In particular, Amazon Alexa retains its own
installed icon rather than receiving Prime Video branding. NOW / Now TV recognizes `now.tv`
and `nowtv`; WOW, Sky Q and the separate Hong Kong Now TV service are not treated as aliases.
Unknown regional IDs continue using their installed artwork. Verify additional aliases against
that TV's inventory before adding them to the allowlist.

The aliases include developer fixtures and community webOS inventories, including
[FHEM's LGTV app IDs](https://github.com/fhem/LGTV_WebOS/blob/master/lib/FHEM/Devices/LGTV/LGTVWebOS.pm),
[a UK webOS inventory](https://github.com/chrispab/AutoHome/blob/master/items/CT_LGWebOS_TV.items),
and the [UK streaming catalogue](https://github.com/tjeffree/webOS-Streaming-Aggregator/blob/main/frontend/src/services/appCatalog.js).
These sources do not establish that every alias is available on every region/firmware combination.

Every curated logo has an app in `demo_inventory()`, including all 27 expanded streaming/music
brands. Open them with `pnpm dev --script apps`, then scroll or search the Apps grid. A unit check
keeps the SVG directory, curated catalogue and demo inventory aligned. `pnpm test:brands` renders
every card through the actual desktop renderer and writes `.build/brands-gallery.png` for visual
review. Like the other smoke scripts, it uses isolated preferences and no TV or real weather API.
The gallery also runs as part of `pnpm test:smoke`. Asset source/licence details are in [assets.md](assets.md).

Live TV (`com.webos.app.livetv`) and Web Browser (`com.webos.app.browser`) also have dedicated
SVG-backed demo cards: a blue/cyan TV display with a live indicator, and a matching globe with a
navigation pointer. Both are original Luma vector artwork. The existing card caption remains available when
names are visible. `Brand::artwork_size()` sets optical sizing per brand, keeping heavy wordmarks
such as Hulu, Joyn and Plex closer to the visual weight of Apple TV, YouTube, Das Erste and ARTE.

Text reads the bundled TTF's advances, kerning and line metrics lazily. Only needed glyph outlines
are tessellated in font-em coordinates. Complete text runs are cached in static GPU buffers,
independent of position/color/drawable density, avoiding a draw call per letter. CJK fonts open
only when needed; there are no glyph bitmap atlases or full-font mesh exports.

MSAA mode requests 4× MSAA, with 2×/single-sample fallbacks if EGL rejects a configuration.
Analytic mode requests a single-sample window. Multisampled text draws
intact glyph fills without inward offsets/fringe triangles, which can cross on very thin
small-text strokes. Single-sample targets use a narrower half-pixel boundary fringe. Glyph
origins/baselines are pixel-aligned per drawable axis. There is no LCD subpixel rendering, font
hinting or complex-script shaping. Inspect `multisample_samples` and `font_antialiasing`:
offscreen captures can be single-sample while the actual window uses MSAA.

Glyph geometry has an 8 MiB/4096-entry CPU cache target and text runs a 32 MiB/512-entry GPU
target. Eviction preserves the current frame, which can temporarily exceed those targets.
Raster artwork has a 48 MiB cache budget. Diagnostics include geometry bytes, triangles, draw
calls and missing/failed artwork/glyphs. Idle screens draw on demand; VSync paces animation.

## Resolution and profiling

### Rendering requests and captures

The shipping default requests 1920×1080 and retains a 1920×1080 logical layout. Drawable-aware
vector coverage follows the actual SDL framebuffer. `--resolution 4k` is an **experimental**
native surface request, not a guaranteed panel-output mode. The TV compositor can constrain or
rescale it. A 4K screenshot or drawable alone does not prove 1:1 panel presentation.

```sh
pnpm dev --resolution 4k --profile-frames 180 --diagnostics .build/profile.json
pnpm dev --script 'home,down' --frames 90 --capture-scale 2 --screenshot .build/home-4k.png
```

`--resolution` also accepts `1080p` or `WIDTHxHEIGHT`. Explicit TV requests use a borderless
window, avoiding SDL desktop-fullscreen overriding the dimensions. `--capture-scale 2` uses a
separate 3840×2160 offscreen target and leaves the actual window unchanged. Font/vector geometry
uses that target's physical coverage rather than enlarging a 1080p image. Optional `@2x` wallpaper
images need both `pnpm assets --4k` and `node tools/package.mjs --4k`; default packages exclude
them even if they remain in an old cache. Source photographic detail still limits sharpness.

### 4K TV investigation

Manual testing on 2026-10-23 used an **LG OLED55C57LA (C5), webOS 26, chip `O22A3`, driver
`lg1kdrm`**. A contributor reported similar corruption on a C3 in
[#25](https://github.com/ecklf/lg-luma/issues/25). Usable native 4K UI presentation has not been
established on these tested TVs.

The C5's default EGLFS JSON reported `1920x1080`, and the active DRM state independently confirmed
a 1080p compositor framebuffer and output mode. The connector advertised `3840x2160`, and
`/proc/lg/fbdev/drm-helper/check_mode` reported `3840x2160: ok` (`7680x4320: err`). FB0 reported
`cfg.max_xres = 1920`, `cfg.max_yres = 1080` and virtual size `1920,2160`, consistent with two
stacked 1080p buffers rather than a 4K framebuffer.

The startup script uses Weston-style arguments and a `weston.ini` with `backend=drm-backend.so`
and `override-output-from-env=true`. Its product environment also generates the EGLFS JSON.
Inspect the actual startup path and DRM state; the JSON alone does not establish which output
configuration is active.

A temporary compositor-level test bind-mounted patched copies of `surface-manager.sh` and
`eglfs_starfish.env`, requesting matching 3840×2160 compositor and primary output geometry, then
restarted `surface-manager-daemon.service`. Systemd scheduled restoration after 90 seconds.

| Observation                  | Default            | Forced 4K test                  |
| ---------------------------- | ------------------ | ------------------------------- |
| Compositor framebuffer       | 1920×1080          | 3840×2160                       |
| Format                       | ABGR8888 with AFBC | ABGR8888 with AFBC              |
| Pitch                        | 7680 bytes         | 15360 bytes                     |
| Plane source and destination | 1920×1080          | 3840×2160                       |
| Active DRM mode              | 1920×1080 at 60 Hz | 3840×2160 at 60 Hz              |
| Visible picture              | Normal             | Flickering lines and corruption |

Restoring the original configuration returned the picture to normal. This was a system compositor
experiment, not a supported Luma setting or installation procedure.

**Mode acceptance, a 4K drawable or screenshot, and even matching 4K framebuffer/plane/CRTC
dimensions do not prove correct panel presentation.** Check the visible output and use a
pixel-resolution test pattern before claiming 1:1 presentation. The corruption's exact cause is
unresolved: FB0's configured maximum and compatibility metadata alone do not establish a physical
hardware limit or explain the failure. These results do not prove that 4K graphics are impossible
on every LG TV; they establish that the tested override does not produce a usable picture on this C5.
An app-side resolution request alone does not resolve that display-path failure.

For read-only inspection in the TV's root SSH shell (paths depend on firmware):

```sh
cat /sys/kernel/debug/dri/0/state
cat /proc/lg/fbdev/status
cat /proc/lg/fbdev/drm-helper/status
cat /proc/lg/fbdev/drm-helper/check_mode
```

**4K video playback is a separate capability.** LG's
[app resolution specifications](https://webostv.developer.lge.com/develop/specifications/app-resolution)
distinguish 1080p graphics from 4K video on UHD TVs. Native media players can route decoded video
through a separate video plane while drawing their UI at 1080p. For example,
[PlxNative documents its default 1080p UI drawable](https://github.com/GLinnik21/plx-native/blob/13bed25004df4c89f8bcba9558b571b8ed962bd0/rust-modules/base/src/surface.rs#L10-L25)
alongside 4K video support. That media path does not demonstrate a usable 4K OpenGL graphics path
for Luma; bypassing the browser does not by itself reconfigure LG's graphics output pipeline.

### Frame profiling

`--profile-frames N` forces redraws and measures swap-to-swap intervals after ten warmup
presentations. Hidden windows pause profiling. Capture stalls are excluded. CPU submission is
not a GPU timestamp, and `--frames` counts event-loop iterations, not presentation rate.
With `--frames`, animations run on a fixed 1/60 s step per event-loop iteration instead of wall
time (`src/clock.rs`), so a smoke budget covers the same animation on any runner. Without VSync
(Xvfb, llvmpipe) the loop free-runs while animating, and a faster machine would otherwise spend
the budget before a transition finishes. `--profile-frames` keeps wall time.
Inspect `window`, `drawable`, `drawable_is_4k`, `last_presented`, `capture` and `profile` in JSON.
Do not infer 60 fps from a successful VSync request; measure actual TV pacing and transitions.
`LUMA_PROFILE_SKIP=wallpaper,image,app,shape,gradient,shadow,text` (or `all`) leaves those draws
out, to find which part of a scene costs GPU time. On an LG webOS 5 TV (Mali-TDVX) the cost
tracks covered area: a full-screen layer takes about 7 ms at 1080p, and MSAA costs nothing
measurable.

### Partial redraw

Each frame redraws only what changed since the frame its back buffer holds. `src/damage.rs`
compares consecutive scenes command by command, realigning after inserted or removed commands
such as a focus ring; the union of changes since the buffer's frame (EGL buffer age) is the only
area cleared and drawn, with a scissor intersected with each clip, and commands entirely outside
it are not laid out. `EGL_KHR_partial_update` also tells the driver which tiles to keep (webOS
Mali); `EGL_EXT_buffer_age` alone still allows the smaller redraw. The EGL entry points come from
the libEGL SDL loaded, and only with a current EGL context, so GLX/CGL/WGL contexts always draw in
full. Unknown buffer ages, changed clip or fade state, a new drawable and screenshot captures draw
the whole frame, as does `LUMA_PARTIAL=0`. The startup log names the mode
(`Luma partial redraw: …`).

`LUMA_DAMAGE_VERIFY=1` compares every frame with the same scene drawn in full offscreen at the
window's sample count and reports differing pixels in the diagnostics (`damage_verify`, with the
full frames as baseline). `--profile-frames` diagnostics include `redrawn_fraction_mean` and a
per-frame `timeline` of interval, compose, render and swap time and redrawn share.

Full-screen translucent fills drawn straight over the wallpaper (the Recents backdrop and
gradient) each map a pixel p to p * (1 - a) + c * a, so together they are p * m + k. The renderer
draws the wallpaper once with m and k per vertex (16 strips keep gradients' colour-alpha products
exact) instead of one full-screen pass per fill. During the Recents crossfade these fills sit
beneath the outgoing interface rather than over it, so they fold in while both fade.

### Idle preparation

The first visit to a page pays for laying out its text, outlining new glyphs, loading logo meshes
and uploading icons. After each inventory, once the screen has been still for 1.5 s, Luma builds
the other pages' scenes without drawing them (`Renderer::prepare`), one page per idle wake; the TV
log reports each (`Luma prewarm: …`). The script action `prewarm` triggers it at once, and the
profiling timeline records first-use work per frame (`glyph_ms`, `vector_ms`, `image_ms`).

## TV startup diagnostics

SAM passes launch metadata as a JSON object in `argv[1]`. The CLI skips that object while leaving
the original process arguments available to SDL/libhelpers. To expose the actual developer-mode
sandbox's stdout/stderr, close Luma and run in the TV's root SSH shell:

```sh
APP=/media/developer/apps/usr/palm/applications/com.ecklf.luma
XDG_RUNTIME_DIR=/tmp/xdg RUST_BACKTRACE=1 /usr/bin/jailer \
  -t native_devmode -i com.ecklf.luma -p "$APP" \
  "$APP/luma" '{"@system_native_app":true,"nid":"com.ecklf.luma"}'
```

Startup prints accepted launch parameters, SDL driver, actual sample count, cumulative stage
timings, first-frame latency and vector-text cache size. Timings begin inside the executable and
exclude SAM/jailer process-launch overhead. The IPK is unpacked at install, not every launch.
A previous vector-font startup on the development TV presented its first frame in 399 ms;
new rendering paths still need their own hardware comparisons. SDL helper `getAppInfo` or
`getSystemSettings` registration calls can report permission warnings; they have not blocked
the verified interface/inventory runs. Normal launch/relaunch behavior should be checked too.

The native adapter registers LS2 connections from the executable and calls `LSCallOneReply`.
Anonymous names avoid collisions with SDL's registration; the hub still applies the executable's
role. `luna-send-pub -a` app impersonation is rejected on the development webOS 26 TV and is not
used; the only `-a` use is the root system-bus `luna-send` that holds a staged Home's lifecycle
registration (see [Home lifecycle on webOS 10/11](#home-lifecycle-on-webos-1011)). Native requests run on one worker's private GLib context with a 6.5-second budget and 2 MiB
reply limit. Errors include URI and the bus refusal. Fixed inventory commands run via elevated
Homebrew Channel `exec`, parsing complete multi-line `luna-send -n 1` JSON replies. Input-read
failures preserve the last valid inputs. No failed inventory read substitutes demo apps on TV.

### Optional native Home integration

`tv/home.sh` and the native Rust `luma-helper` executable are packaged beside `luma`. They are a root-only, explicit integration
for webOS 10/25 and 11/26 (and webOS 5/6, see below). `enable` stages a native manifest with ID `com.webos.app.home` and
symlinks to the installed executable, libraries and assets under `/var/lib/luma-home/app`, then
bind-mounts it over `/usr/palm/applications/com.webos.app.home`. SAM rereads that one app through
`blockSystemApp` followed by `releaseBlockSystemApp`, including `forceBlockSystemApp: false` when
the TV schema requires it. The script verifies the reported manifest before stopping the previous
Home process. It attempts to restore LG Home if applying the mount or refreshing SAM fails.

Before staging, `enable` closes the normal Luma instance with `closeByAppId` and waits up to five
seconds for it to leave the running list. The TV's `close` endpoint identifies a process, so passing
the package ID there can return `no app matched by pid`. If closing reports an error, the script
rechecks the running list and proceeds only if the normal instance has already exited.

Root-shell calls use `luna-send -n 1` on both firmware versions, with bounded stdout/stderr
capture and operation-specific diagnostics for empty or invalid replies. `-t 1` is timing mode:
it logs the payload to stderr and writes timing summaries to stdout, not a JSON response.

The `enable` shell entry point prints the change summary and requires a `y`/`n` confirmation
before creating the state directory, acquiring the lock or invoking the installer. Only `y`/`Y`
proceeds. `n`/`N` or end-of-input cancels, and other input repeats the prompt. The boot hook and
`apply` remain non-interactive, and `disable`/`restore` remain available without confirmation.

The native CLI accepts SAM's Home `nid` and gives SDL the matching Home surface identity. The
executable itself stays at its installed path for LS2 authentication, `$ORIGIN/lib`, asset discovery
and preference selection. Opening Luma's normal tile while the Home mount is active redirects to
the Home instance. The scripts do not create a browser wrapper or change the package's app ID.

`64-luma-home` reapplies the mount from a detached boot hook. The existing TV Home Auto Launch
and Quick Start+ settings determine whether power-on opens Home. Until the hook applies, LG Home
can still appear. This integration does not change those settings or install a remote-key filter.
`disable` (also `restore`) removes the mount, refreshes LG's manifest, stops the native Home
instance and removes the boot hook. The copied restore scripts remain in `/var/lib/luma-home`
even if the installed package is removed. Disable the standalone `65-luma` cold-boot hook before
enabling this integration.

Build and script checks cannot establish native Home compatibility. Manually verify Home, Back
out of the last app, standby wake with the TV's chosen Quick Start+ setting, a full cold boot,
saved preferences and restoring LG Home. The broader webOS 6+ app compatibility statement does
not establish Home-replacement support on older firmware.

### Running in place of LG's Home executable (webOS 5)

On webOS 5 the manifest takeover above does not apply: SAM restarts a v1 native Home on every
Home launch. A root integration can instead bind-mount the installed `luma` executable over LG's
`/usr/bin/com.webos.app.home`. SAM then starts it with LG's own Home manifest and LS2 role, which
is bound to that executable path. Luma detects this (`platform::runs_as_lg_home`) and:

- disables SDL's own app registration, which cannot reach the bus under LG's Home role, and
  registers with SAM itself through `registerApp` (lifecycle v2) on its own LS2 connection.
  `luna::subscribe` waits up to 10 s for SAM's first reply (`registered`); a failed connection,
  call or refused registration is a startup error, so Luma exits and SAM starts Home afresh.
  Home-replacement mode also claims SDL-webOS's Home key, returning directly to the Home tab
  when SAM does not relaunch an already-foreground Home. Normal app-tile launches do not claim it.
  `relaunch` returns to Luma's Home tab, dismisses overlays, raises the window and refreshes
  the inventory. Keyboard edits are saved before leaving their subpage. `close` and a later
  `returnValue: false` (registration lost) quit. SAM keeps the process across Home launches and
  standby (`pause`, reason `keepAlive`) instead of restarting it;
- resolves `assets/`, `lib/` (extra RPATH) and the splash directory from the installed package,
  since `current_exe()` names LG's path. Production Homebrew Channel installs under
  `/media/cryptofs`, developer installs under `/media/developer` and system installs under
  `/usr/palm/applications` share the lookup list in `tv/install-directories.txt`. Library search
  retains `$ORIGIN/lib` first and includes all three locations before Rust startup. Native
  discovery validates the package manifest and matches the executable by device/inode, so assets
  and splash updates use the source package even when developer and production copies both exist.
  A missing or unrelated source package is an explicit startup error;
- writes its stderr to `/tmp/luma-home.log`, because SAM discards a system app's output;
- skips the per-icon elevated-service fallback for installed artwork: as root every file is
  readable, so that fallback would only spend seconds per icon to fail the same way.

`tv/home.sh` chooses this method (`"method": "executable"` in `state.json`) on webOS 5/6 when LG
Home's manifest is native, starts `file:///usr/bin/com.webos.app.home` and declares
`nativeLifeCycleInterfaceVersion: 2`. `enable` and the boot hook bind-mount the installed
executable over that path, stop the previous Home process and open Home if it was in front (or
on `enable`). Luma in front within about 15 s is the success condition, with one repeated launch
after 6 s, because SAM can accept the first launch right after the old process stopped without
starting anything; otherwise the mount is removed again. A mount whose source is Luma's executable
counts as its own also after a package update (`//deleted`) and is replaced; any other mount over
LG's executable is refused. `disable` unmounts (lazily while Home runs), stops Luma and reopens LG
Home if Home was in front. While mounted, `/var/run/luma-home` names Luma, so a normal
tile launch redirects to the Home instance: the tile's jail sees `/usr/bin` without the mount but
shares `/var/run`, which is volatile like the mount itself. LG's file stays underneath, and a cold boot
without the hook gives LG's Home back. Verified on an LG 49UN7100
(webOS 5): Home key to Luma in about 0.4 s instead of a restart of about 2 s.

### Home lifecycle on webOS 10/11

With the staged manifest (see [Optional native Home integration](#optional-native-home-integration)), SAM starts Luma's own executable for `com.webos.app.home`, but
never as a registered app: on every Home key it kills Luma and starts it again. Running in place
of LG's executable is not an option here, because LG Home's role
(`/usr/share/luna-service2/roles.d/com.webos.app.home.app.json`) is app-ID based
(`"appId": "com.webos.app.home"`, names `com.webos.app.home-*`), not bound to a path, and Luma's
role only allows `com.ecklf.luma` or an anonymous name.

The staged Home runs as root, and as root the system-bus `luna-send -a` may act for an app ID.
So when `options.webos_home` is set outside LG's executable path and the effective user is root,
Luma calls `luna::subscribe_as_app`, which runs `app_subscription::subscribe` with a child
`luna-send -a com.webos.app.home -i` that holds
`registerNativeApp`. SAM expects lifecycle v1 for this manifest and refuses `registerApp` with
"unmatched method with nativeLifeCycleInterfaceVersion". v1 names its events in `message` rather
than `event`; `platform::Lifecycle::from_reply` reads both and feeds the same `relaunch`/`close`
handling as v2.

- Startup does not wait for SAM. Until it answers, Home is simply unregistered.
- No answer within 10 s, a refused answer or the child ending before an answer kills and reaps
  the child and leaves Home unregistered, as before this integration: SAM restarts it per Home
  key. Once SAM has accepted the registration, losing it (a later `returnValue: false` or the
  child ending) quits Luma, as on the v2 path, so SAM starts Home afresh.
- The child gets `SIGKILL` when the thread that started it (Luma's main thread) ends
  (`PR_SET_PDEATHSIG`, re-checked against the parent PID after arming). A registration left with
  nobody handling its events leaves the Home key dead and a black screen after leaving an app.
- On `relaunch` Luma restores, shows and raises its window (on both lifecycle paths). If a
  relaunch arrives while Luma is not both focused and visible, `platform::ForegroundCheck` waits
  3 s for `FocusGained` or a remote key, which only reach the focused surface. `Shown` and
  `Restored` do not count, because `SDL_ShowWindow` emits `Shown` itself whether or not the raise
  reached the screen. Without confirmation, Luma assumes it cannot bring itself forward on this
  firmware: it creates `/var/lib/luma-home/no-lifecycle` and ends the registration, which quits
  it, so SAM starts Home afresh. While that file exists, Luma does not register and Home behaves
  as before. Delete it to try again.
- A setup error after the child started (e.g. a thread cannot be created) kills and reaps the
  child before returning the error, so no registration is left behind while Luma keeps running.
- stderr goes to `/tmp/luma-home.log` in this mode too. Replies are logged as
  `Luma lifecycle: …`, the helper's end as `Luma app subscription ended (was registered: …)`,
  and window visibility and focus changes as `Luma window: …`.
- `app_subscription` has no LS2 dependency, so its host tests run it against fake `luna-send`
  scripts: replies and noise, refusal, silence, an early exit, a registration lost later, and
  failed thread starts. `ForegroundCheck` has its own tests, including relaunch → `Shown` without
  focus.
- SDL's own app registration (`SDL_WEBOS_REGISTER_APP`) is left as it was for the staged
  manifest. It does not register Home there (SAM restarted Luma per Home key with it enabled),
  and disabling it would change startup on a path without a reason seen on a TV.

Verified on an LG OLED65C36LC (webOS 26, 11.2.0-3701): without registration the Luma PID changes
on every Home key. With it, the PID stays the same across Home presses and Back out of apps, SAM
sends `relaunch` with `launchType: homeKey`, Luma comes to the front, and it registers again after
a cold boot. SDL's own registration logs `Failed to register app: Failed to get
nativeLifeCycleInterfaceVersion` at startup there, without effect. That the compositor sends
`FocusGained` after such a relaunch, which the watchdog relies on, shows in the `Luma window:` log.

## Manual extra-app preview

```sh
pnpm dev --test-extra-apps 120
```

On the TV, add `--test-extra-apps 120` after the JSON argument in the jailed command above.
This explicitly appends up to 300 labeled non-launchable cards to a successful real inventory,
using its apps as artwork/category templates. It uses `scroll-preview-preferences.json`,
initialized from normal choices once. Favoriting test cards exercises long Home grids without
changing normal favorites. Ctrl+C then a normal launch restores real inventory/preferences.
The placeholder cards are never a fallback for an inventory failure.

## Storage, weather and languages

TV preferences live in an app-owned persistent directory outside the installed executable's
payload. Startup probes `/media/developer/<appid>/`, `/media/developer/temp/<appid>/` and
`/media/internal/.<appid>/`, selecting a directory the jailed app can actually create and write.
Production `/media/cryptofs` installations prefer internal storage. `/media/developer/temp` is
on the persistent developer partition, not the reboot-cleared `/tmp`. No settings are placed
in `/tmp` and no system/app-payload permissions are widened. Root Home takeover reuses these
same app-owned directories, preferring existing app-owned preferences over root-owned fallback
files left by an earlier takeover. It does not merge or delete duplicate preferences. Preference
and weather-cache saves retain the directory owner's UID/GID so the normal app can still read
them after Home is disabled. Symlink storage directories remain rejected. The executable Home
method uses its source installation for storage precedence and legacy migration, not LG's
`/usr/bin` executable path. Existing JSON preferences and a
valid weather cache from beside this installation's old executable are imported once, without
overwriting the chosen destination. Invalid existing preferences still report an error.
Startup prints `Luma persistent storage:` with the chosen path. Desktop preview retains the legacy
`$XDG_CONFIG_HOME/luma-native/preferences.json` path, or `~/.config/luma-native/preferences.json`,
so existing preview choices survive flattening. `--preferences PATH` isolates a manual run.
Invalid preference files report an error rather than being overwritten. Writes use atomic
temporary-file renames; use one instance per preferences file. Browser local storage is not
migrated into native JSON preferences.

Weather Location searches cities/postcodes only on explicit Search. Forecasts run when Home is
visible, the widget enabled and a city configured. ureq/rustls uses embedded certificate roots,
an eight-second timeout, no redirects/proxy and 256 KiB reply limits. Search/forecast/cache writes
have separate workers and generation IDs, avoiding stale responses overwriting current state.
Fresh Celsius readings are reused for 15 minutes; valid saved data lasts up to six hours and
failure retries after five minutes. Unit changes convert unrounded cached Celsius immediately.
Removing a city clears its cache. The `.weather.json` sidecar uses Unix-seconds timestamps.
Mock servers accept only literal loopback HTTP origins and are refused in TV builds.

Movie and show search follows the Apps search text once a Movie of the Night key is stored.
`streaming::Incremental` waits for three characters and a 350 ms pause, answers repeats from
memory and drops replies for text that has since changed. `streaming_search::Service` runs each
search on its own worker thread, reading `streaming-keys.json` per search so new keys apply at
once, and caches every API reply under `streaming-cache/` beside the preferences. OK launches the
picked app with `deeplink` parameters through `com.webos.applicationManager/launch`. Scripted
captures can wait with `await-titles`. Pre-filling `streaming-cache/` from
`tests/fixtures/streaming` with a placeholder key renders results without network access.

`assets/messages.json` supplies Dutch, English, French, German, Italian, Polish, Portuguese (Brazil),
Russian, Simplified Chinese, Spanish, Swedish, Traditional Chinese and Vietnamese, including locale formatting
and weather labels. App/device names and saved city names remain real data. No geolocation,
runtime font/translation services or telemetry are used. Weather attribution: Open-Meteo,
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

## Update availability

On interactive desktop or TV startup a separate worker checks
`https://luma.elischleifer.com/repo.json` once per process automatically.
**Settings → System → Check again** retries on the same worker, with one request in flight at a time.
It only reads the `com.ecklf.luma` package/manifest version and compares numeric major/minor/patch
components with the compiled version. Equal, older, malformed or prerelease versions do not offer
updates. HTTP is HTTPS-only with bounded redirects, a ten-second timeout, a 256 KiB reply limit and
no proxy; failures are logged without interrupting launch and the status card offers retry.
Scripted captures and simulated update previews do not contact the release server. Interactive desktop checks
use the same endpoint and worker as TV. The card shows the current version and only reports
**No newer version found** after a successful check, never while unchecked or offline.
A newer version shows an **Update available** notice and a **Settings → System** card with both
versions. Its **Install {version}** button explicitly starts installation on interactive TV runs.
The read-only checker never downloads an IPK. A separate installer worker rechecks release metadata,
requires the exact offered version, and accepts only this project's ARM release assets with a bounded
size and SHA-256. It downloads into a private random directory and verifies both before handing off
to `tv/self-update.sh`, detached through Homebrew Channel's root service into a systemd unit.
The handoff escapes dollar signs for systemd as well as quoting for the
exec service's shell, so systemd cannot expand the embedded script before `/bin/sh` runs it.
The installer locks, stages and re-verifies a root-owned copy, waits for installation completion and
checks the installed version before reopening Luma. Home takeover refreshes from the new package;
failed reapplication attempts verified LG Home recovery. Atomic attempt-specific results and a log
persist in `/var/lib/luma-update`. The card displays progress, ignores repeated presses while busy,
and returns to **Check again** after failure. On restart the installer worker observes an unfinished
attempt or reports its persisted failure. Desktop and scripted/simulated runs cannot install.

Preview the update card without a network request:

```sh
cargo run -- --test-update-version 99.0.0 --script settings,down,down,down,down,right
# Alternatively: LUMA_TEST_UPDATE_VERSION=99.0.0 cargo run
```

The flag overrides the environment variable and accepts the current or a newer numeric version.
Use the current version to preview the up-to-date card and **Check again** without HTTP. It is logged as
a simulated update, stored only in memory, and disables the public feed check even on TV. Open
**Settings → System** to see it. The install action is rejected locally without HTTP, Luna calls or
package installation in these previews.

## Compatibility

Luma's native startup, rendering and app/input discovery have been verified on rooted webOS 26.
External testing confirms compatibility with webOS 6 and newer. On webOS 26 use elevated Homebrew Channel
0.8.0-rc1+ for `lunabus.query`. Root SSH execution does not establish the normal jailed caller
permissions; check TV-menu startup, launching/reopening and remote/pointer behavior manually.

[webOS Homebrew Can I Use](https://www.webosbrew.org/develop/caniuse) lists firmware library
versions. Its [library guide](https://www.webosbrew.org/develop/guides/library-version) maps
internal releases to marketing names: 7/22, 8/23, 9/24, 10/25, 11/26. SDL-webOS's newer
[`2.30.12-webos.6`](https://github.com/webosbrew/SDL-webOS/releases/tag/release-2.30.12-webos.6)
reports hotplug verification on webOS 3.4, 4 and 10. That is subsystem evidence for a newer
prerelease than the pinned `.5` library, not a full Luma compatibility guarantee.

## Checks and CI

Rust behavior tests cover navigation, preferences, localization, rendering geometry and weather
state without a TV. `tests/` scripts exercise actual desktop framebuffers and mock HTTP responses
for navigation, persistence, pointer input, translations/CJK, weather limits/cache/races and
profiling. They contact no live weather APIs or television. On Linux:

```sh
pnpm format:check
pnpm check
pnpm test
pnpm assets
xvfb-run -a pnpm test:smoke
```

CI prepares checks, unit tests, native assets and the desktop binary once. Four independent
jobs run navigation/rendering, the brand gallery, mocked weather and translations in parallel,
each with its own Xvfb display, scratch preferences and diagnostic artifact. They consume a
same-run tar archive that preserves the binary's executable mode and use `LUMA_SMOKE_SKIP_BUILD=1`
to avoid compiling the same executable again. Local smoke commands still build by default.
The ARM package job runs alongside these suites using the shared assets, cross-links with a
pinned Linux SDK, inspects/normalizes the actual IPK and generates checksums/feed. The existing
**Rust checks, rendering, and native package** check remains as an aggregate that requires every
preparation, smoke and package job to succeed. It uploads release/rendering artifacts on PRs,
manual runs and Release runs. CI does not run again after a merge: PRs are the gate. `pnpm check` also validates synchronized release versions,
and `pnpm test:release` covers version synchronization, the minor/major release bump and the R2
feed mirror in an isolated fixture. Host CI cannot prove hardware compatibility,
small-text quality or presentation frame rate.

### PR test IPKs

`.github/workflows/pr-test-ipk.yml` runs **PR Test IPK** whenever a PR is opened, reopened or
receives new commits, including fork PRs and drafts. It automatically builds the exact PR head
commit and uploads the verified ARM IPK and `sha256sums`. One bot comment is created or updated
with the full commit hash, workflow run and artifact download link. Failed or cancelled builds
update the same comment with their status; superseded commits cannot overwrite the latest head's
comment. Downloads are GitHub artifact ZIPs, require signing in and expire after 14 days.

The workflow uses only the `opened`, `synchronize` and `reopened` PR events, without a deployment
environment or required reviewers. Each new commit cancels the previous run. It becomes active
after it is merged into the PR's base branch; reopen an existing PR to request its first build,
or rerun a workflow to rebuild the same commit.

Only the build job checks out PR code. It has read-only repository access, no persisted
checkout credentials and no shared build caches. A separate job with comment-writing permission
uses the trusted base-branch workflow and GitHub's artifact API without checking out PR code.
The package keeps the normal `com.ecklf.luma` identity and version, so installing it replaces
the existing Luma app. These builds are for manual TV verification; the ordinary CI checks still
run independently.

## Releases

Pull requests need no release bookkeeping: keep the existing app version and don't add version
or changelog files. Merging never publishes. The **Release** workflow picks the version, and
each GitHub release's notes are generated from the pull requests merged since the previous release
(see [Release notes](#release-notes)), so a clear PR title and description are what end up in them.

### Merge gate

CI runs on pull requests (and manual dispatch), not after merging. Protect `main` so nothing lands
untested: **Settings → Rules → Rulesets → New ruleset → New branch ruleset**, enforcement
**Active**, target **Include default branch**, then enable:

- **Require status checks to pass** with **Require branches to be up to date before merging**,
  adding the check **Rust checks, rendering, and native package**. "Up to date" means a PR is
  re-tested against the current `main` before it can merge, so CI results are never stale.
  (Merge queues do the same with less waiting but are only offered for organization-owned
  repositories; `ci.yml` already runs on `merge_group`, so a queue can be enabled after a move.)
- **Block force pushes** and **Restrict deletions**.

Leave **Require a pull request before merging** off: the Release workflow fast-forwards `main` to
its version commit directly. Before doing so it validates that exact commit with `ci.yml` and marks
it with a successful **Rust checks, rendering, and native package** commit status, so the ruleset
accepts it. Any other push to `main` without that passing check is rejected.

### Publishing a release

Run **Actions → Release → Run workflow** on `main` (`.github/workflows/release.yml`) and choose:

- **bump**: `auto` (the default) releases the next minor version, for example 2.19.0 → 2.20.0.
  `major` releases the next major version, for example 2.19.0 → 3.0.0.

The workflow then:

1. **Bump version.** `pnpm version:bump minor|major` updates `package.json` and synchronizes
   `appinfo.json`, the root `Cargo.toml`, the `luma` entry in `Cargo.lock` and the versioned IPK
   examples in `readme.md`. The result is committed as `Release vX.Y.Z` on a temporary
   `release/vX.Y.Z` branch. An existing tag stops the run.
2. **Validate.** Calls `ci.yml` on that commit: checks, unit tests, the four desktop smoke suites and
   the verified ARM IPK and Homebrew feed.
3. **Publish GitHub release.** Writes the [release notes](#release-notes), fast-forwards `main` to
   the release commit (refusing if `main` moved meanwhile), creates the `vX.Y.Z` GitHub release from
   that run's artifacts and deletes the temporary branch.
4. **Publish to Cloudflare R2.** See [Release hosting](#release-hosting-cloudflare-r2).

If bumping or validation fails, `main`, tags and the published feed are untouched: fix the problem
and run Release again. If only the R2 step fails, re-run that job or run **Publish to Cloudflare R2**
manually with the tag. `ci.yml` on its own (PRs, manual dispatch) only validates.
No additional token is needed; the built-in token pushes the release commit.

`pnpm version:check` checks synchronization without writing files. `pnpm version:sync` repairs
metadata from the current `package.json` version without calculating a bump. `pnpm version:bump`
is reserved for the Release workflow, and `node tools/sync-version.mjs --set X.Y.Z` for the Nightly
workflow.

### Release notes

`tools/release-notes.mjs` writes the notes for releases and nightlies. It asks GitHub for the list
of pull requests merged since the previous stable release and puts a **What's new** section above
that list. Each pull request is grouped by the files it changed:

- **On the TV**: the app (`src/`, `tv/`, `tv-helper/src/`, `assets/`, `overlay/`,
  `vector-geometry/`, `appinfo.json`; test files don't count).
- **Home Assistant**: `custom_components/`, `hacs.json`.
- **Behind the scenes**: everything else (CI, tooling, tests, docs), title only.

The first two carry the pull request's own summary: the first paragraph, or the top-level bullets
(at most six), of the description's `## Summary` section, or of the text before its first heading.
Wrapped bullet lines are kept, nested bullets dropped, and any paragraph or bullet longer than
500 characters is cut at a word. If the notes would go over GitHub's 125,000-character limit, the
summaries are left out (titles only). So the PR title and summary are what users read; write
them for someone holding the remote. No
external service or secret is involved; a GitHub API failure stops the run before anything is
published.

### Nightly builds

**Actions → Nightly** (`.github/workflows/nightly.yml`) runs every day at 09:37 UTC, and can be run
by hand on `main`. It builds only when `main` has commits since the last release and there is no
nightly for them yet; otherwise it stops after a few seconds.

A nightly is versioned `X.Y.N`: `X.Y.0` is the last release (the version in `main`'s
`package.json`) and `N` is the number of commits on `main` since it. Releases are always `X.Y.0`,
so a nightly sorts after the release it builds on and before the next one; a TV on a nightly
upgrades to the next release like any other. The workflow:

1. Commits the nightly version on a temporary `nightly/vX.Y.N` branch (`main` is never changed).
2. Runs the full `ci.yml` validation on that commit, like a release.
3. Publishes a `vX.Y.N` GitHub **prerelease** titled "Luma X.Y.N (nightly)", with release notes
   covering everything since the last release, then deletes all but the newest five nightly
   prereleases and their tags.
4. Mirrors it to R2 and updates `https://luma.elischleifer.com/nightly/repo.json`, deleting the R2
   folders of pruned nightlies. The stable `/repo.json` is never touched.

To follow nightlies on a TV, add `https://luma.elischleifer.com/nightly/repo.json` as a repository
in Homebrew Channel. Luma's own updater keeps reading the stable feed, so it offers the next release
and never a nightly. A failed nightly changes nothing; the next night retries if `main` hasn't moved.

Homebrew Channel's details page reads `packages[].shortDescription` (or `fullDescriptionUrl`),
not `manifest.appDescription`. The feed generator writes the root app metadata description into
`shortDescription` and `manifest.appDescription`. It also copies `assets/homebrew-description.html`
to `dist/description.html` and sets the package's `fullDescriptionUrl` to that version's release
asset. Homebrew Channel prefers this HTML description when the URL is present. CI uploads the
HTML alongside the feed and IPK, so the feed and its description must be published together.

Key modules: `state.rs` focus/preferences, `view.rs` geometry, `renderer.rs` GPU caches/draws,
`font.rs` lazy metrics/outlines, `vector.rs` GPU mesh upload, `platform.rs` worker/inventory,
`luna.rs` LS2, `weather*.rs` scheduling/HTTPS/cache, `storage.rs` atomic JSON, and `main.rs` SDL loop.

### Home-key routing on webOS 5/6 (#109)

`home.sh status` reports the selected `homeId` and installed Home candidates. `mounted`,
`enabled` and `bootHook` describe integration files, not a successful remote Home-key test;
`homeKeyVerified` therefore remains `null` (unknown). LG Home can be an overlay, so foreground app
identity alone is not sufficient evidence that Luma rendered or received the Home key.

If diagnostics on the affected TV establish that Home opens `com.webos.app.fullhome`, disable
existing takeover first, then explicitly select that target:

```sh
sh /var/lib/luma-home/home.sh disable
sh /media/developer/apps/usr/palm/applications/com.ecklf.luma/home.sh enable /media/developer/apps/usr/palm/applications/com.ecklf.luma com.webos.app.fullhome
```

Use the installed package directory on production installations. The selected target is saved
for boot, package reapply and restore; no input remapping or Linux key-code assumption is used.
The original `home` target remains the default. Press Home from another app to verify, then test
disable restores LG's dashboard. This is a targeted compatibility option, not confirmation of
the unreported cause of #109. The reporter's device still needs to establish which Home app
receives the key. Do not infer support for webOS 4 from this option.

### Native TV helper

The `tv-helper/` workspace crate ports the former `tv/*.cjs` operations. Its `luma-helper` binary
contains no GUI initialization, SDL dependency or persistent service. Root Home synchronizes its
splash directly through the same Rust library on the platform worker. Normal jailed launches
retain Homebrew Channel as the privilege boundary, executing the installed helper with individually
quoted arguments. The helper retains bounded file reads, combined stdout/stderr caps, timeouts,
protected application IDs and completion-based uninstall replies. `luna-send` is still used for
root-side LS2 operations; Node is no longer required by these helpers or by `tv/self-update.sh`,
which reads its two JSON fields with POSIX tools.

`home.sh` retains its explicit confirmation and filesystem lock. Enabling Home copies both that
script and the native helper into `/var/lib/luma-home`, so recovery remains available after the
source package is removed. When upgrading an existing integration from the interpreter helper,
run the packaged `home.sh enable` again to replace its copied recovery/boot files. Configurations
from the former scripts keep their method, installation and original stock manifest. Both `home`
and `fullhome` executable targets are supported; the latter requires matching native lifecycle
support from #142. Manifest takeover remains limited to webOS
10/11. The optional `tv-input` subcommand implements the #124 registration/verification contract;
its Settings integration is still tracked by that separate PR and requires `supportGIP`.

The former VM tests for interpreter code are replaced by filesystem and service fixtures in
`tv-helper/src/tests.rs`. They do not call a TV, real LS2 service or network. Host helper executables
reject TV operations; only ARM webOS builds can perform them. Use `cargo test -p luma-tv-helper`
for focused checks. The workspace tests include the helper, and ARM builds/package verification
require both native binaries with executable permissions. `tests/home-wrapper.test.mjs` runs
`home.sh` itself through absolute and relative boot-hook symlinks in a temporary root, so the copied
helper is found beside the script's target rather than the `init.d` link.

On an LG 49UN71006LB running webOS 5.6.2, a manual comparison of the same Homebrew Channel icon
request returned identical image bytes. Measured from a TV-local parent process around each
complete child invocation (startup, file reads, JSON/hex output and exit), the old Node 8.12.0
helper took a median 2.588 seconds over three samples; the native ARM helper took 0.0128 seconds
over seven samples (range 0.0120–0.0208 seconds). This is about 203 times faster for that helper
invocation, saving about 2.575 seconds. It excludes Homebrew transport and is not a measurement of
the full launcher startup or every helper operation. No memory-usage comparison was made.

Manual verification on that TV covered native package installation, Home enable/disable, the
actual HOME button from YouTube, explicit fullhome launch and normal-tile redirect, splash
synchronization, copied recovery files, protected uninstall rejection, and complete removal of a
new disposable test app through status 31. The previous build, preferences and integration policy
were restored after testing. Manifest takeover on webOS 10/11 is covered by fixtures, but was not
tested on hardware in this run; standby and cold boot were not tested.

## Release hosting (Cloudflare R2)

Releases are mirrored to a Cloudflare R2 bucket served at `https://luma.elischleifer.com`, so TVs can use
`https://luma.elischleifer.com/repo.json` as their Homebrew Channel repository and the in-app updater
(`src/update.rs`) reads the same feed. Only IPKs under `https://luma.elischleifer.com/releases/` are
accepted for self-update.

Bucket layout:

```text
repo.json                  latest feed, asset URLs rewritten to luma.elischleifer.com (60 s cache)
nightly/repo.json          latest nightly feed (60 s cache)
releases/vX.Y.Z/*.ipk      immutable release assets, sha256sums, description, icon
releases/vX.Y.Z/repo.json  that release's mirrored feed
```

Nightlies (`vX.Y.N`, `N > 0`) use the same `releases/` folders. When the nightly workflow mirrors
with `channel: nightly`, it updates `nightly/repo.json` instead of the root feed (only for the
newest nightly prerelease, so re-mirroring an older one never rolls TVs back), and deletes any
patch-version folder that is no longer a GitHub release, which removes pruned nightlies but never
a stable release.

After the Release workflow publishes a GitHub release, its `mirror` job calls
`.github/workflows/publish-r2.yml`. It downloads the release assets, verifies `sha256sums`, rewrites
the feed with `tools/mirror-feed.mjs` and uploads to R2 through its S3-compatible API (versioned assets
first, root feed last). If a zone purge token is configured it also purges `/repo.json` from
Cloudflare's cache; otherwise the 60-second `Cache-Control` bounds staleness. Run **Publish to
Cloudflare R2** manually to backfill or re-mirror a tag; only the latest GitHub release may replace
the root feed.

### One-time Cloudflare setup

1. **R2 → Create bucket**, e.g. `luma` (location: Automatic).
2. **Bucket → Settings → Custom Domains → Connect Domain** → `luma.elischleifer.com`. Cloudflare creates
   the DNS record and certificate. Leave the `r2.dev` public URL disabled.
3. **R2 → Manage API tokens → Create API token** (Account API token): permission **Object Read & Write**,
   scoped to the `luma` bucket only. Copy the Access Key ID and Secret Access Key, and note the account
   ID shown on the R2 overview page.
4. Optional cache purge: **My Profile → API Tokens → Create Token** with **Zone → Cache Purge** for
   `elischleifer.com`; note the zone ID from the domain's Overview page.

### GitHub configuration

Settings → Secrets and variables → Actions:

| Kind                | Name                     | Value                                   |
| ------------------- | ------------------------ | --------------------------------------- |
| Secret              | `R2_ACCESS_KEY_ID`       | R2 token Access Key ID                  |
| Secret              | `R2_SECRET_ACCESS_KEY`   | R2 token Secret Access Key              |
| Variable            | `CLOUDFLARE_ACCOUNT_ID`  | Cloudflare account ID                   |
| Variable            | `R2_BUCKET`              | bucket name, e.g. `luma`                |
| Secret (optional)   | `CLOUDFLARE_PURGE_TOKEN` | zone cache-purge token                  |
| Variable (optional) | `CLOUDFLARE_ZONE_ID`     | `elischleifer.com` zone ID              |
| Variable (optional) | `LUMA_PUBLIC_URL`        | default `https://luma.elischleifer.com` |

Then run **Publish to Cloudflare R2** once with an empty tag to mirror the current release, and check
`curl -sI https://luma.elischleifer.com/repo.json`.
