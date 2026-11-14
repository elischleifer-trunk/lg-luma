# Home Assistant

Luma can show a camera in a corner of the TV **over whatever is playing**, for example your UniFi
Protect doorbell when someone rings, and short notifications ("The washer is done"). Home
Assistant calls the TV; the TV never needs your Home Assistant password.

> [!IMPORTANT]
> The overlay relies on webOS's `overlay` window type for a transparent web app
> (`com.ecklf.luma.overlay`). It is not a documented LG contract. The same approach is used by
> [LG Camera Overlay](https://github.com/mbsintegrations/LG-Camera-Overlay), but DRM-protected
> apps, some firmware and some models may behave differently. Please report what works on yours.

## What you get

- **`luma.show_camera`**: a camera card in a corner of the screen with a title, an optional second
  line and a countdown. **OK** on the remote shows the camera full screen; **Back** closes it;
  otherwise it closes after its duration.
  Choose whether the show keeps playing underneath or pauses until the card closes.
- **`luma.dismiss`** and a **Dismiss overlay** button: close the card from Home Assistant, for
  example when the door opens.
- **`luma.notify`** and a **Toast** notify entity: a standard webOS toast over any app.
- **A Cameras app** in Luma: every Home Assistant camera in a full-screen grid. Arrows choose a
  camera, **OK** shows it full screen, **Back** returns to the grid and then closes it. Hold
  **OK** on a camera to move, rename or hide it; the settings button shows hidden cameras again.
  Five or more cameras show three across and two rows at a time; scroll down for the rest.
- **Show example camera card** and **Show example toast** buttons: press them on the TV's device
  page to see what each popup looks like, before writing any automation. The example card shows a
  picture of a front door that comes with the integration, so it works without a camera.

## Set up

1. **On the TV**, open **Luma → Settings → System → Home Assistant** and turn it on. Luma shows a
   QR code and the address Home Assistant should use (for example `192.168.1.50:5862`). The first
   time, Luma also installs its small overlay app from its own package. This needs Homebrew
   Channel's root service, like Luma's other TV features. To see the QR code again later, choose
   **Add to Home Assistant** on the same page.
2. **Install the integration** (you need [HACS](https://hacs.xyz)):
   - **From the QR code:** scan it with your phone. It opens HACS on your Home Assistant with
     **Luma for LG webOS** ready to download, through the
     [My Home Assistant](https://my.home-assistant.io) link
     `https://my.home-assistant.io/redirect/hacs_repository/?owner=elischleifer-trunk&repository=lg-luma&category=integration`.
     The first time, My Home Assistant asks for your Home Assistant's address.
   - **By hand in HACS:** add `https://github.com/elischleifer-trunk/lg-luma` as a custom repository of
     type _Integration_, then download **Luma for LG webOS**.
   - **Without HACS:** copy `custom_components/luma` from this repository into your Home Assistant
     `config/custom_components/` folder.

   Restart Home Assistant.

3. Home Assistant usually discovers the TV on its own, using the same announcement as the built-in
   LG webOS TV integration. Otherwise, add **Luma for LG webOS** under **Settings → Devices &
   services** and enter the TV's address. When you already use the LG webOS TV integration, its
   address and name are suggested.
4. The TV shows a **six-digit pairing code**. Type it into Home Assistant. If Luma is on screen,
   the code replaces the QR code in the **Add to Home Assistant** dialog, which opens by itself
   if needed, and stays until you pair. Codes expire after two minutes, and five wrong codes
   cancel the request.

5. To check it works, open the TV under **Settings → Devices & services → Luma for LG webOS** and
   press **Show example camera card** and **Show example toast**. The card closes after 15
   seconds, or press **Back** on the remote.

Turning **Home Assistant** off in Luma closes the port and forgets every paired server. Home
Assistant then asks you to pair again the next time it is needed. Your camera names, order and
hidden cameras are kept, and come back when the same Home Assistant pairs again. Removing the integration in Home
Assistant also unpairs it from the TV when the TV is on.

## Camera images

The TV loads camera images itself, so the address must be reachable from the TV.

- **A camera entity** (`camera:`) is easiest. Home Assistant passes the TV its own camera proxy
  addresses, signed with the camera's short-lived access token: a motion-JPEG stream with snapshot
  fallback, or snapshots only with `mode: snapshot`. Home Assistant uses its **local network URL**
  (**Settings → System → Network**); pass `base_url` if the TV needs another one. Camera tokens
  rotate every few minutes, so keep snapshot-mode overlays shorter than that.
- **go2rtc** (built into Home Assistant, or as an add-on) gives a smoother, lighter feed from a
  camera's low-resolution channel. Pass its URLs directly:

  ```yaml
  stream_url: http://192.168.1.10:1984/api/stream.mjpeg?src=front_door
  snapshot_url: http://192.168.1.10:1984/api/frame.jpeg?src=front_door
  ```

For a camera entity, the card plays Home Assistant's HLS stream: the camera's own video at its
full frame rate, muted, decoded by the TV, a second or two behind real time (see
[Live video in the card](#live-video-in-the-card)). Otherwise it shows motion-JPEG or JPEG;
`mode: stream` keeps a camera entity on motion-JPEG, so the TV's video decoder stays with the show
you are watching.

## Cameras app

While **Home Assistant** is on, Luma lists a **Cameras** app with your other apps. Favorite it to
keep it on Home. Opening it shows every camera entity Home Assistant has (unavailable ones are
skipped, at most 32) in a grid of snapshots that refresh every couple of seconds. Choose a camera
and press **OK** to see it full screen as live video; **Back** returns to the grid, and **Back**
again closes it.

Full screen, a camera that can stream (most RTSP cameras: Generic, ONVIF, Reolink, UniFi Protect
and others) plays Home Assistant's HLS stream: the camera's own video at its full frame rate,
decoded by the TV, a second or two behind real time. This needs Home Assistant's **Stream**
integration, which `default_config` includes. The video starts within a few seconds; to make it
start at once for a camera you open often, turn on **Preload stream** in that camera entity's
settings in Home Assistant, which keeps its stream running. Other cameras, and any camera while its stream is
starting or if it cannot play, show Home Assistant's motion-JPEG stream, which for most camera
types is only about two pictures a second. Its badge says **Live** or **Snapshots**: if that
stream fails or freezes, Luma switches to snapshots, several a second.

### Rewinding and history

A full-screen camera can be rewound. A bar along the bottom shows how far back you can go, with
live at the right end:

| Key                                                  | What it does                                                                                             |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| **Left** / **Right**                                 | 10 seconds back or forward; hold to go faster (a minute, then five minutes, then half an hour at a time) |
| **OK**                                               | Pause, and play again                                                                                    |
| **Up** / **Down**                                    | The previous or next thing UniFi Protect saw (UniFi Protect cameras)                                     |
| **Rewind** / **Fast-forward**                        | Play backward or forward at 2×, 4×… up to 64×                                                            |
| **Right** to the end, or **Down** past the last mark | Back to live                                                                                             |
| **Back**                                             | Back to the grid                                                                                         |

The TV keeps the last two minutes of every camera you watch. **UniFi Protect cameras go back 24
hours**, into what Protect recorded, including before you opened the camera. Scrubbing shows
Protect's picture from each moment, and pressing **OK** (or stopping on a moment while playing)
plays Protect's recorded video. Coloured marks on the bar show what Protect saw: people (blue),
vehicles (orange), animals (green), packages (purple), doorbell rings (bright green) and other
motion (grey). A camera set to record only on motion has gaps; **Nothing recorded at this time**
shows there.

This needs Home Assistant's **UniFi Protect** integration and, for the marks, Home Assistant's
**Recorder** (part of `default_config`), which keeps the history of the camera's motion and
detection sensors. Nothing else needs setting up: the Luma integration gives the TV a link per
camera that checks the camera's access token, then hands the TV a short-lived link that Home
Assistant signs for the UniFi Protect integration's own recording views, the same way Home
Assistant's media browser plays Protect recordings. The TV never holds Home Assistant or Protect
credentials.

### Arranging the grid

To arrange the grid, hold **OK** on a camera: **Move** lets you move it with the arrows (**OK**
when done), **Hide camera** takes it off the grid, and **Camera settings** opens settings, which
the settings button at the top right also opens (press **Up** from the first row). There you can
show hidden cameras again and turn **Live video** off, so full-screen cameras use snapshots,
which suits cameras whose stream is slow in Home Assistant. Luma remembers the arrangement.

Nothing needs setting up: the integration sends the TV its camera list when it starts and again
whenever a camera changes, including every few minutes when Home Assistant rotates camera access
tokens, so the URLs the TV holds keep working. Like camera overlays, the TV loads the images from
Home Assistant's **local network URL**. The TV keeps the list in memory only; after it restarts,
Home Assistant notices within a minute and sends the list again. With several Home Assistant
servers paired, the app shows all of their cameras.

## Doorbell example

UniFi Protect doorbells have an `event` entity that changes on every ring:

```yaml
automation:
  - alias: Doorbell on the TV
    triggers:
      - trigger: state
        entity_id: event.front_door_doorbell
        not_from: unavailable
    actions:
      - action: luma.show_camera
        data:
          camera: camera.front_door_medium_resolution_channel
          message: Someone is at the door
          duration: 45
          position: top-right
          size: medium
          # true pauses the show until the card closes, then resumes it.
          pause: "{{ is_state('input_boolean.pause_tv_for_doorbell', 'on') }}"

  - alias: Close the doorbell card when the door opens
    triggers:
      - trigger: state
        entity_id: binary_sensor.front_door_contact
        to: 'on'
    actions:
      - action: luma.dismiss
```

Leave out `config_entry_id` to show the camera on every paired TV, or pick one TV in the action
editor.

| Field                        | Default     | Notes                                                    |
| ---------------------------- | ----------- | -------------------------------------------------------- |
| `camera`                     |             | A camera entity; or give `stream_url` / `snapshot_url`.  |
| `stream_url`, `snapshot_url` |             | Absolute `http(s)` URLs the TV can load.                 |
| `mode`                       | `video`     | `video` (HLS), `stream` (motion-JPEG) or `snapshot`.     |
| `refresh`                    | `1`         | Seconds between snapshots, 0.25–30.                      |
| `title`, `message`           | camera name | Up to 80 and 200 characters.                             |
| `duration`                   | `30`        | Seconds, 5–600.                                          |
| `position`                   | `top-right` | Any corner.                                              |
| `size`                       | `medium`    | `small` (480 px), `medium` (640 px) or `large` (800 px). |
| `pause`                      | `false`     | Pause playback until the card closes.                    |

### Live video in the card

With a camera entity that can stream (most RTSP cameras, including UniFi
Protect), Home Assistant starts the camera's HLS stream when the card is requested. The card shows
the motion-JPEG while the video starts, which usually takes a few seconds, then switches to the
video. A camera that cannot stream, a video that does not play within 15 seconds, or one that
stops, keeps or goes back to the motion-JPEG. Like the Cameras app, this needs Home Assistant's
**Stream** integration. The video is muted.

The card's video uses the TV's video decoder while something else is playing. Whether the show
underneath keeps playing smoothly depends on the TV model and firmware. If it does not, use
`mode: stream`, or `pause: true`. Please report what works on yours.

To peek at a camera any time, as if the doorbell rang, call the same action from a script and put
it on a dashboard:

```yaml
script:
  peek_front_door:
    alias: Peek at front door
    icon: mdi:doorbell-video
    sequence:
      - action: luma.show_camera
        data:
          camera: camera.front_door_medium_resolution_channel
          message: Live view
```

A second ring while the card is showing replaces it. When the first ring paused the show, it stays
paused until the replacement closes.

**Pause** pauses the playing media pipeline and resumes the same one afterwards. Apps that play
through webOS's media service (most streaming apps) pause; Live TV and apps with their own player
may keep playing. To pause such apps another way, call `media_player.media_pause` on the LG webOS TV
entity before `luma.show_camera`.

## Streaming search keys

Luma's streaming search uses a Movie of the Night key (where shows stream), a TMDB API Read
Access Token (how prominent they are) and, optionally, an MDBList API key (Rotten Tomatoes and
IMDb scores on each poster). Enter them once in Home Assistant: **Settings → Devices &
services → Luma → Configure**. Each new key is checked with its service when you save, then sent
to the TV. Home Assistant is their source of truth: it sends them again if the TV loses them, for
example after a reset, or if they were changed on the TV. Clearing a field in Home Assistant
leaves the TV's key in place. Without Home Assistant, enter them on the TV under **Settings →
System → Streaming search keys**, which shows a QR code for your phone.

## Keeping Luma and the integration in step

Home Assistant's device page for the TV shows which Luma version it runs. When the TV and the
integration no longer fit together, both say which one to update:

- **Luma on the TV is too old:** Home Assistant shows **Update Luma on _TV_** under **Settings →
  Repairs**. Update Luma from its settings on the TV.
- **The integration is too old:** Home Assistant shows **Update the Luma integration for _TV_**
  under Repairs, Luma shows a notice, and **Settings → System → Home Assistant** on the TV names
  the server. Update **Luma for LG webOS** in HACS and restart Home Assistant.

Both notices go away on their own once the update is installed. They compare API levels, not
release versions, so an ordinary release of either side never raises them.

## While the card is showing

The card has key focus, so **Back** closes it and **OK** shows the camera full screen: its live
video, or its motion-JPEG feed when it has none, with no countdown. Full screen, **Back** or **OK**
closes it, and a paused show resumes. Other remote keys, such as play/pause, may not reach
the app underneath until it closes. How webOS routes keys to overlay windows depends on firmware.

## What the TV reports

The TV tells Home Assistant what went wrong and how smoothly Luma is running. Home Assistant
keeps a live connection to it while the TV is on, and catches up on anything it missed after it
restarts or the TV wakes up.

- **Home Assistant's log** (**Settings → System → Logs**) shows Luma's warnings and errors, such
  as a camera that has no HLS stream or a camera card that did not open, under
  `custom_components.luma.tv`. To see everything Luma records, including what went right (startup
  timings, how long each camera card took to open), add this to `configuration.yaml`:

  ```yaml
  logger:
    logs:
      custom_components.luma.tv: info
  ```

- **Diagnostic sensors** on the TV's device page:
  - **Frame time**: the 95th percentile of how long a frame took to draw, in milliseconds.
  - **Dropped frames**: frames that missed a refresh while Luma was animating.
  - **Memory** and **CPU**: how much Luma uses.
  - **Last problem**: the last warning or error.

  Luma reports these once a minute while it is drawing and every ten minutes when idle. Home
  Assistant's history graphs them over time.

- **A `luma_event` event** for each entry, with `level` (`info`, `warning` or `error`), `area`
  (such as `cameras`, `overlay` or `inventory`), `message`, `time`, `device_id` (the TV's device)
  and `config_entry_id`.
  Automations can use it, for example to send you a notification when Luma logs an error.
- **Download diagnostics** on the TV's device page saves one file with everything together: the
  history Home Assistant kept (the last 1,000 entries and 12 hours of performance), and what the
  TV still holds. Attach it when you report a problem. Tokens and keys are removed from it.

The TV keeps its last 500 entries in memory. If Home Assistant is away long enough for more than
that to pile up, the log says some were lost. If Luma stops because of an internal error, it
reports the error the next time it starts.

## Security

- The port (`5862`) only exists while **Home Assistant** is on in Luma. It answers only devices on
  the TV's private network.
- Every action needs a token that was issued by entering the on-screen code. The TV stores only
  SHA-256 digests of tokens.
- Requests carrying a browser `Origin` header are refused, so web pages cannot use the API. The
  one exception is the overlay app reporting that it closed, which is accepted only from the TV
  itself and only with that overlay's random nonce.
- Camera URLs must be `http` or `https`. The TV loads them as images only.
- Streaming search keys are stored on the TV in a file only Luma can read. The API never sends
  them back: `status` only says which keys the TV holds.
- What the TV reports about itself goes only to paired servers. Unpairing a server ends its
  event stream.

## API

Home Assistant uses this API. You can script it too: send JSON bodies, with
`Authorization: Bearer TOKEN` on every endpoint except `info` and the two pairing steps.
Home Assistant also sends `X-Luma-Integration: LEVEL`, its integration's API level; requests
without it count as level 1. `api` in a reply is the TV's own API level and `min_integration`
the lowest integration level it works with.

| Endpoint                  | Body                                                           | Reply                                                                                        |
| ------------------------- | -------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `GET /api/info`           |                                                                | `id`, `name`, `version`, `api`, `min_integration`                                            |
| `POST /api/pair`          | `name`                                                         | `pairing_id`; the TV shows a code                                                            |
| `POST /api/pair/confirm`  | `pairing_id`, `code`                                           | `token`, `id`                                                                                |
| `DELETE /api/pair`        |                                                                | Unpairs this token                                                                           |
| `GET /api/status`         |                                                                | `version`, `api`, `min_integration`, `overlay`, `overlay_ready`, `cameras`, `streaming_keys` |
| `POST /api/overlay`       | As `luma.show_camera`, using URLs                              | `session`, `paused`                                                                          |
| `DELETE /api/overlay`     |                                                                | Closes the card, resumes playback                                                            |
| `POST /api/notify`        | `message` (up to 300 characters)                               | Shows a webOS toast                                                                          |
| `POST /api/cameras`       | `cameras`: `id`, `name`, `stream_url`, `snapshot_url`          | Replaces the Cameras app's list                                                              |
| `PUT /api/streaming-keys` | `motn`, `tmdb`, `mdblist`; empty or missing keeps the TV's key | Which keys the TV holds                                                                      |
| `GET /api/events`         | `?after=` the last event's `id`                                | Luma's log as server-sent events (below)                                                     |
| `GET /api/diagnostics`    |                                                                | Everything the TV kept of its log, and its state                                             |

`GET /api/events` stays open. It sends a `hello` event (`boot`, `version`, `uptime_s`, and
`restarted` when Luma restarted since the `after` cursor), then what the TV kept after the cursor,
then each new entry as it happens. A `log` event's data has `seq`, `time` (Unix milliseconds),
`level`, `area` and `message`. A `metrics` event has the same fields plus `data` with `presented`,
`dropped`, `work_p50_ms`, `work_p95_ms`, `work_max_ms`, `rss_mb`, `cpu_percent` and `uptime_s`.
Each of these events has an `id` (`boot:seq`). Pass the last one as `after` to resume. A `missed`
event says that entries after the cursor were already dropped. A comment line arrives every 15
seconds. Each server has at most one stream: connecting again replaces the earlier one.

## Troubleshooting

- **Home Assistant cannot find the TV:** check that the TV is on, Luma is running (it is when it
  replaces LG Home) and **Home Assistant** is on in Luma's settings.
- **The card says "Camera unavailable":** open the camera URL from another device on the TV's
  network. With a camera entity, check Home Assistant's local network URL or pass `base_url`.
- **Nothing appears over an app:** the TV may not allow overlay windows over that app (DRM) or on
  that firmware. `luma.notify` toasts still work there.
- **"The overlay app is still being installed":** the first install after turning the setting on
  takes a few seconds. Try again; if it persists, turn **Home Assistant** off and on in Luma.
