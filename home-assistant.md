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
  line and a countdown. **Back** on the remote closes it early; otherwise it closes after its
  duration. Choose whether the show keeps playing underneath or pauses until the card closes.
- **`luma.dismiss`** and a **Dismiss overlay** button: close the card from Home Assistant, for
  example when the door opens.
- **`luma.notify`** and a **Toast** notify entity: a standard webOS toast over any app.
- **A Cameras app** in Luma: every Home Assistant camera in a full-screen grid. Arrows choose a
  camera, **OK** shows it full screen, **Back** returns to the grid and then closes it. Hold
  **OK** on a camera to move or hide it; the settings button shows hidden cameras again.
- **Show example camera card** and **Show example toast** buttons: press them on the TV's device
  page to see what each popup looks like, before writing any automation. The example card shows a
  picture of a front door that comes with the integration, so it works without a camera.

## Set up

1. **On the TV**, open **Luma → Settings → General** and turn on **Home Assistant**. Luma shows a
   QR code and the address Home Assistant should use (for example `192.168.1.50:5862`). The first
   time, Luma also installs its small overlay app from its own package. This needs Homebrew
   Channel's root service, like Luma's other TV features. To see the QR code again later, choose
   **Add to Home Assistant** below the setting.
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
4. The TV shows a **six-digit pairing code**. Type it into Home Assistant. Codes expire after two
   minutes, and five wrong codes cancel the request.

5. To check it works, open the TV under **Settings → Devices & services → Luma for LG webOS** and
   press **Show example camera card** and **Show example toast**. The card closes after 15
   seconds, or press **Back** on the remote.

Turning **Home Assistant** off in Luma closes the port and forgets every paired server. Home
Assistant then asks you to pair again the next time it is needed. Removing the integration in Home
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
integration, which `default_config` includes. Other cameras, and any camera while its stream is
starting or if it cannot play, show Home Assistant's motion-JPEG stream, which for most camera
types is only about two pictures a second. Its badge says **Live** or **Snapshots**: if that
stream fails or freezes, Luma switches to snapshots, several a second.

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

## While the card is showing

The card has key focus, so **Back** closes it. Other remote keys, such as play/pause, may not reach
the app underneath until it closes. How webOS routes keys to overlay windows depends on firmware.

## Security

- The port (`5862`) only exists while **Home Assistant** is on in Luma. It answers only devices on
  the TV's private network.
- Every action needs a token that was issued by entering the on-screen code. The TV stores only
  SHA-256 digests of tokens.
- Requests carrying a browser `Origin` header are refused, so web pages cannot use the API. The
  one exception is the overlay app reporting that it closed, which is accepted only from the TV
  itself and only with that overlay's random nonce.
- Camera URLs must be `http` or `https`. The TV loads them as images only.

## API

Home Assistant uses this API. You can script it too: send JSON bodies, with
`Authorization: Bearer TOKEN` on every endpoint except `info` and the two pairing steps.

| Endpoint                 | Body                                                  | Reply                                 |
| ------------------------ | ----------------------------------------------------- | ------------------------------------- |
| `GET /api/info`          |                                                       | `id`, `name`, `version`, `api`        |
| `POST /api/pair`         | `name`                                                | `pairing_id`; the TV shows a code     |
| `POST /api/pair/confirm` | `pairing_id`, `code`                                  | `token`, `id`                         |
| `DELETE /api/pair`       |                                                       | Unpairs this token                    |
| `GET /api/status`        |                                                       | `overlay`, `overlay_ready`, `cameras` |
| `POST /api/overlay`      | As `luma.show_camera`, using URLs                     | `session`, `paused`                   |
| `DELETE /api/overlay`    |                                                       | Closes the card, resumes playback     |
| `POST /api/notify`       | `message` (up to 300 characters)                      | Shows a webOS toast                   |
| `POST /api/cameras`      | `cameras`: `id`, `name`, `stream_url`, `snapshot_url` | Replaces the Cameras app's list       |

## Troubleshooting

- **Home Assistant cannot find the TV:** check that the TV is on, Luma is running (it is when it
  replaces LG Home) and **Home Assistant** is on in Luma's settings.
- **The card says "Camera unavailable":** open the camera URL from another device on the TV's
  network. With a camera entity, check Home Assistant's local network URL or pass `base_url`.
- **Nothing appears over an app:** the TV may not allow overlay windows over that app (DRM) or on
  that firmware. `luma.notify` toasts still work there.
- **"The overlay app is still being installed":** the first install after turning the setting on
  takes a few seconds. Try again; if it persists, turn **Home Assistant** off and on in Luma.
