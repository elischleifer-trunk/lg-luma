"""Luma for LG webOS: camera overlays and notifications on TVs running the Luma launcher.

Home Assistant calls the TV; the TV never needs Home Assistant credentials. For a camera entity,
the service passes the TV Home Assistant's camera proxy URLs with the camera's short-lived access
token, which the overlay app on the TV loads directly.

Each paired TV also receives the list of every camera, again as signed proxy URLs, for Luma's
Cameras app. The list is sent again whenever a camera changes, which includes every access token
rotation, so the TV always holds working URLs. Cameras that can stream also get an `hls_url`: the
TV asks it, with the same camera token, to start Home Assistant's HLS stream (the camera's own
H.264 video at full frame rate, which the TV decodes in hardware) and receives its address.

The TV also reports on itself: Luma's log and performance arrive live over an event stream (see
`events`), go to Home Assistant's log and diagnostic sensors, and fill the diagnostics download.

Cameras from UniFi Protect also get a `history_url` into what Protect has recorded. The TV asks
it, with the same camera token, for a picture at any past moment, a stretch of recorded video, or
when Protect saw motion. Pictures and video come from the UniFi Protect integration's own views:
this one only checks the token and redirects to a short-lived path signed by Home Assistant (as
its media browser does), so the TV never holds Home Assistant or Protect credentials. Motion
comes from Home Assistant's own history of the camera's Protect sensors.
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from functools import partial
from pathlib import Path
from typing import Any

import voluptuous as vol
from aiohttp import web
from homeassistant.components.http import HomeAssistantView
from homeassistant.components.http.auth import async_sign_path
from homeassistant.config_entries import ConfigEntry, ConfigEntryState
from homeassistant.const import CONF_HOST, CONF_PORT, EVENT_STATE_CHANGED, Platform
from homeassistant.core import HomeAssistant, ServiceCall, callback
from homeassistant.exceptions import (
    ConfigEntryAuthFailed,
    HomeAssistantError,
    ServiceValidationError,
)
from homeassistant.helpers import config_validation as cv
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.event import async_track_time_interval
from homeassistant.helpers.network import NoURLAvailableError, get_url
from homeassistant.helpers.typing import ConfigType
from homeassistant.util import dt as dt_util

from . import streaming_keys
from .api import LumaAuthError, LumaClient, LumaError
from .const import (
    CONF_TOKEN,
    DEFAULT_PORT,
    DOMAIN,
    POSITIONS,
    SERVICE_DISMISS,
    SERVICE_NOTIFY,
    SERVICE_SHOW_CAMERA,
    SIZES,
)
from .events import LumaEvents
from .events import store as events_store

_LOGGER = logging.getLogger(__name__)
PLATFORMS = [Platform.BUTTON, Platform.NOTIFY, Platform.SENSOR]
CONFIG_SCHEMA = cv.config_entry_only_config_schema(DOMAIN)

ATTR_CONFIG_ENTRY = "config_entry_id"
ATTR_CAMERA = "camera"
ATTR_STREAM_URL = "stream_url"
ATTR_SNAPSHOT_URL = "snapshot_url"
ATTR_MODE = "mode"
ATTR_BASE_URL = "base_url"
HLS_VIEW_URL = "/api/luma/stream/{entity_id}"
HISTORY_VIEW_URL = "/api/luma/history/{entity_id}"
PROTECT = "unifiprotect"
PROTECT_SNAPSHOT = "/api/unifiprotect/snapshot/{nvr}/{camera}/{timestamp}"
PROTECT_VIDEO = "/api/unifiprotect/video/{nvr}/{camera}/{start}/{end}"
# A redirect only has to be followed; the signature is checked when the request starts.
SIGNED_FOR = timedelta(minutes=1)
# How far back the TV may ask, the longest stretch of video at once, and of motion.
HISTORY_DAYS = 31
MAX_VIDEO = timedelta(minutes=15)
MAX_EVENTS = timedelta(hours=48)
MAX_EVENT_COUNT = 2000
# What a Protect sensor saw, from its own key: the first match names it.
EVENT_KINDS = ("person", "vehicle", "animal", "package", "doorbell", "motion")
# CameraEntityFeature.STREAM; the camera component is imported only when a stream is requested.
CAMERA_STREAM = 2
# Camera changes arrive in bursts (one per camera at each token rotation); send the list once.
CAMERAS_PUSH_DELAY = 2.0
# A doorbell card waits at most this long for Home Assistant to start a camera's HLS stream.
HLS_START_TIMEOUT = 5.0
MAX_CAMERAS = 32
# Luma keeps cameras in memory only; this often, check whether a restarted TV needs the list.
CAMERAS_CHECK_INTERVAL = timedelta(minutes=1)

# A picture of a front door that the example camera card shows, so it works without a camera.
EXAMPLE_IMAGE_URL = "/api/luma/example.jpg"
EXAMPLE_IMAGE = Path(__file__).parent / "example.jpg"


@dataclass
class LumaData:
    client: LumaClient
    events: LumaEvents


type LumaConfigEntry = ConfigEntry[LumaData]


async def async_setup_entry(hass: HomeAssistant, entry: LumaConfigEntry) -> bool:
    client = LumaClient(
        async_get_clientsession(hass),
        entry.data[CONF_HOST],
        entry.data.get(CONF_PORT, DEFAULT_PORT),
        entry.data[CONF_TOKEN],
    )
    # The TV may be off; only a definite "not paired" stops setup. Calls fail individually.
    try:
        await client.status()
    except LumaAuthError as err:
        raise ConfigEntryAuthFailed("Luma no longer knows this Home Assistant server") from err
    except LumaError as err:
        _LOGGER.debug("Luma at %s is not reachable yet: %s", client.host, err)
    events = LumaEvents(hass, entry, client)
    entry.runtime_data = LumaData(client, events)
    _track_cameras(hass, entry, client)
    # Home Assistant holds the streaming search keys: send them now and whenever they change.
    entry.async_create_background_task(
        hass, streaming_keys.push(client, dict(entry.options)), f"{DOMAIN} keys {entry.entry_id}"
    )
    entry.async_on_unload(entry.add_update_listener(_options_updated))
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    # After the platforms, so the TV's device exists for the first events' device_id.
    await events.async_start()
    return True


async def _options_updated(hass: HomeAssistant, entry: LumaConfigEntry) -> None:
    await streaming_keys.push(entry.runtime_data.client, dict(entry.options))


def camera_list(hass: HomeAssistant) -> list[dict[str, str]]:
    """Every available camera with signed proxy URLs, for Luma's Cameras app."""
    try:
        base_url = local_url(hass)
    except ServiceValidationError:
        return []
    # HLS needs Home Assistant's stream integration (part of default_config).
    streams = "stream" in hass.config.components
    cameras = []
    for state in sorted(hass.states.async_all("camera"), key=lambda state: state.name.casefold()):
        token = state.attributes.get("access_token")
        if not token or state.state == "unavailable":
            continue
        stream, snapshot = _proxy_urls(base_url, state.entity_id, token)
        camera = {
            "id": state.entity_id,
            "name": state.name[:80],
            "stream_url": stream,
            "snapshot_url": snapshot,
        }
        features = int(state.attributes.get("supported_features", 0))
        if streams and features & CAMERA_STREAM:
            path = HLS_VIEW_URL.format(entity_id=state.entity_id)
            camera["hls_url"] = f"{base_url}{path}?token={token}"
        if protect_entry(hass, state.entity_id):
            path = HISTORY_VIEW_URL.format(entity_id=state.entity_id)
            camera["history_url"] = f"{base_url}{path}?token={token}"
        cameras.append(camera)
    return cameras[:MAX_CAMERAS]


def _track_cameras(hass: HomeAssistant, entry: LumaConfigEntry, client: LumaClient) -> None:
    """Send the TV the camera list now and after camera changes, coalescing bursts."""
    pending = False

    async def push() -> None:
        nonlocal pending
        await asyncio.sleep(CAMERAS_PUSH_DELAY)
        pending = False
        try:
            await client.cameras(camera_list(hass))
        except LumaError as err:
            # The TV may be off; the next camera change sends the list again.
            _LOGGER.debug("Could not send cameras to %s: %s", client.host, err)

    @callback
    def schedule() -> None:
        nonlocal pending
        if pending:
            return
        pending = True
        entry.async_create_background_task(hass, push(), f"{DOMAIN} cameras {entry.entry_id}")

    @callback
    def is_camera(event_data) -> bool:
        return str(event_data.get("entity_id", "")).startswith("camera.")

    @callback
    def changed(_event) -> None:
        schedule()

    async def check(_now) -> None:
        # After the TV or Luma restarts it reports no cameras; send the list again then.
        # The same status says whether the TV lost or replaced the streaming search keys.
        cameras = not pending and bool(camera_list(hass))
        has_keys = any(streaming_keys.keys(entry.options))
        if not (cameras or has_keys):
            return
        try:
            status = await client.status()
        except LumaError:
            return
        if cameras and status.get("cameras") == 0:
            schedule()
        if streaming_keys.needs_push(dict(entry.options), status):
            await streaming_keys.push(client, dict(entry.options))

    entry.async_on_unload(hass.bus.async_listen(EVENT_STATE_CHANGED, changed, is_camera))
    entry.async_on_unload(
        async_track_time_interval(hass, check, CAMERAS_CHECK_INTERVAL, cancel_on_shutdown=True)
    )
    schedule()


async def async_unload_entry(hass: HomeAssistant, entry: LumaConfigEntry) -> bool:
    return await hass.config_entries.async_unload_platforms(entry, PLATFORMS)


async def async_remove_entry(hass: HomeAssistant, entry: ConfigEntry) -> None:
    """Tell the TV to forget this server's token; best effort, the TV may be off. The history
    of what the TV reported goes too."""
    await events_store(hass, entry.entry_id).async_remove()
    client = LumaClient(
        async_get_clientsession(hass),
        entry.data[CONF_HOST],
        entry.data.get(CONF_PORT, DEFAULT_PORT),
        entry.data.get(CONF_TOKEN),
    )
    try:
        await client.unpair()
    except LumaError as err:
        _LOGGER.debug("Could not unpair from %s: %s", client.host, err)


SHOW_CAMERA_SCHEMA = vol.All(
    vol.Schema(
        {
            vol.Optional(ATTR_CONFIG_ENTRY): vol.All(cv.ensure_list, [cv.string]),
            vol.Optional(ATTR_CAMERA): cv.entity_id,
            vol.Optional(ATTR_STREAM_URL): cv.url,
            vol.Optional(ATTR_SNAPSHOT_URL): cv.url,
            vol.Optional(ATTR_MODE, default="video"): vol.In(["video", "stream", "snapshot"]),
            vol.Optional(ATTR_BASE_URL): cv.url,
            vol.Optional("title"): vol.All(cv.string, vol.Length(max=80)),
            vol.Optional("message"): vol.All(cv.string, vol.Length(max=200)),
            vol.Optional("duration", default=30): vol.All(
                vol.Coerce(float), vol.Range(min=5, max=600)
            ),
            vol.Optional("refresh", default=1): vol.All(
                vol.Coerce(float), vol.Range(min=0.25, max=30)
            ),
            vol.Optional("position", default="top-right"): vol.In(POSITIONS),
            vol.Optional("size", default="medium"): vol.In(SIZES),
            vol.Optional("pause", default=False): cv.boolean,
        }
    ),
    cv.has_at_least_one_key(ATTR_CAMERA, ATTR_STREAM_URL, ATTR_SNAPSHOT_URL),
)
DISMISS_SCHEMA = vol.Schema({vol.Optional(ATTR_CONFIG_ENTRY): vol.All(cv.ensure_list, [cv.string])})
NOTIFY_SCHEMA = vol.Schema(
    {
        vol.Optional(ATTR_CONFIG_ENTRY): vol.All(cv.ensure_list, [cv.string]),
        vol.Required("message"): vol.All(cv.string, vol.Length(min=1, max=300)),
    }
)


def _targets(hass: HomeAssistant, call: ServiceCall) -> list[LumaConfigEntry]:
    """The chosen TVs, or every loaded TV when none is chosen."""
    wanted = call.data.get(ATTR_CONFIG_ENTRY)
    entries = [
        entry
        for entry in hass.config_entries.async_entries(DOMAIN)
        if entry.state is ConfigEntryState.LOADED and (wanted is None or entry.entry_id in wanted)
    ]
    if not entries:
        raise ServiceValidationError("No loaded Luma TV matches this call")
    return entries


def local_url(hass: HomeAssistant, base_url: str | None = None) -> str:
    """How the TV reaches Home Assistant: base_url, or Home Assistant's local network URL."""
    if base_url is None:
        try:
            # The TV is on the local network: prefer the internal address, allowing a bare IP.
            base_url = get_url(hass, allow_external=False, allow_cloud=False, allow_ip=True)
        except NoURLAvailableError as err:
            raise ServiceValidationError(
                "Set Home Assistant's local network URL, or pass base_url"
            ) from err
    return base_url.rstrip("/")


def camera_urls(hass: HomeAssistant, entity_id: str, base_url: str | None) -> tuple[str, str]:
    """Home Assistant's MJPEG and snapshot proxy URLs for a camera, signed with its token.

    Camera access tokens rotate every few minutes. An MJPEG stream keeps working once
    connected; snapshot polling stops working after the token rotates, so keep snapshot
    overlays to a few minutes.
    """
    state = hass.states.get(entity_id)
    if state is None or not entity_id.startswith("camera."):
        raise ServiceValidationError(f"{entity_id} is not a camera")
    token = state.attributes.get("access_token")
    if not token:
        raise ServiceValidationError(f"{entity_id} is unavailable")
    return _proxy_urls(local_url(hass, base_url), entity_id, token)


def _proxy_urls(base_url: str, entity_id: str, token: str) -> tuple[str, str]:
    return (
        f"{base_url}/api/camera_proxy_stream/{entity_id}?token={token}",
        f"{base_url}/api/camera_proxy/{entity_id}?token={token}",
    )


async def card_hls_url(hass: HomeAssistant, data: dict[str, Any]) -> str | None:
    """In video mode, start the camera's HLS stream and return its absolute address.

    The card plays it at the camera's full frame rate and keeps the MJPEG and snapshot URLs as
    fallbacks, so a camera that cannot stream (or no stream integration) just shows those.
    """
    camera = data.get(ATTR_CAMERA)
    if data[ATTR_MODE] != "video" or not camera or "stream" not in hass.config.components:
        return None
    state = hass.states.get(camera)
    if state is None or not int(state.attributes.get("supported_features", 0)) & CAMERA_STREAM:
        return None
    base_url = local_url(hass, data.get(ATTR_BASE_URL))
    # The stream is a bonus: however it fails or however slow it is, the card still goes out.
    try:
        async with asyncio.timeout(HLS_START_TIMEOUT):
            path = await _request_stream(hass, camera)
    except Exception as err:
        _LOGGER.debug("No HLS stream for %s, showing motion-JPEG: %r", camera, err)
        return None
    return f"{base_url}{path}"


def overlay_payload(hass: HomeAssistant, data: dict[str, Any]) -> dict[str, Any]:
    stream = data.get(ATTR_STREAM_URL)
    snapshot = data.get(ATTR_SNAPSHOT_URL)
    title = data.get("title")
    if camera := data.get(ATTR_CAMERA):
        camera_stream, camera_snapshot = camera_urls(hass, camera, data.get(ATTR_BASE_URL))
        stream = stream or camera_stream
        snapshot = snapshot or camera_snapshot
        if title is None and (state := hass.states.get(camera)):
            title = state.name
    if data[ATTR_MODE] == "snapshot":
        stream = None
        if snapshot is None:
            raise ServiceValidationError("Snapshot mode needs a camera or snapshot_url")
    payload = {
        "stream_url": stream,
        "snapshot_url": snapshot,
        "title": title,
        "message": data.get("message"),
        "duration": data["duration"],
        "refresh": data["refresh"],
        "position": data["position"],
        "size": data["size"],
        "pause": data["pause"],
    }
    return {key: value for key, value in payload.items() if value is not None}


async def _each(hass: HomeAssistant, entries: list[LumaConfigEntry], action) -> None:
    failures = []
    for entry in entries:
        try:
            await action(entry.runtime_data.client)
        except LumaAuthError:
            entry.async_start_reauth(hass)
            failures.append(f"{entry.title}: pair again")
        except LumaError as err:
            failures.append(f"{entry.title}: {err}")
    if failures:
        raise HomeAssistantError("; ".join(failures))


class ExampleImageView(HomeAssistantView):
    """Serves the example camera card's picture. The TV loads it without credentials."""

    url = EXAMPLE_IMAGE_URL
    name = "api:luma:example"
    requires_auth = False

    async def get(self, request: web.Request) -> web.FileResponse:
        return web.FileResponse(EXAMPLE_IMAGE, headers={"Cache-Control": "max-age=86400"})


def _camera(hass: HomeAssistant, entity_id: str):
    """The camera entity, or None when it is missing or off."""
    try:
        from homeassistant.components.camera.helper import get_camera_from_entity_id
    except ImportError:  # pragma: no cover - older Home Assistant
        from homeassistant.components.camera import (
            _get_camera_from_entity_id as get_camera_from_entity_id,
        )
    try:
        return get_camera_from_entity_id(hass, entity_id)
    except HomeAssistantError:
        return None


async def _request_stream(hass: HomeAssistant, entity_id: str) -> str:
    """Starts the camera's HLS stream and returns its path."""
    from homeassistant.components.camera import async_request_stream

    return await async_request_stream(hass, entity_id, "hls")


def protect_entry(hass: HomeAssistant, entity_id: str) -> str | None:
    """The UniFi Protect config entry of a Protect camera, which records it, or None."""
    entry = er.async_get(hass).async_get(entity_id)
    if entry is None or entry.platform != PROTECT or not entry.config_entry_id:
        return None
    if PROTECT not in hass.config.components:
        return None
    return entry.config_entry_id


def _moment(value: str | None) -> datetime | None:
    """A Unix time in seconds from the TV, within Protect's reach and not in the future."""
    try:
        moment = datetime.fromtimestamp(int(value or ""), UTC)
    except (TypeError, ValueError, OverflowError, OSError):
        return None
    now = datetime.now(UTC)
    if moment > now + timedelta(minutes=1) or moment < now - timedelta(days=HISTORY_DAYS):
        return None
    return moment


def _iso(moment: datetime) -> str:
    return moment.replace(microsecond=0).isoformat()


def sensor_kind(key: str) -> str | None:
    """What a Protect sensor reports, from its own key (`motion`, `smart_obj_person`,
    `doorbell`…), or None for one that reports nothing the timeline shows (`is_dark`…)."""
    return next((kind for kind in EVENT_KINDS if kind in key), None)


def _sensor_key(entry: er.RegistryEntry, camera_object_id: str) -> str:
    """A Protect entity's own key, without the device's name, which may contain any word:
    the unique ID after the device's MAC address, else the entity ID after the camera's name."""
    if entry.unique_id and "_" in entry.unique_id:
        return entry.unique_id.split("_", 1)[1].lower()
    object_id = entry.entity_id.split(".", 1)[1]
    return object_id.removeprefix(f"{camera_object_id}_")


def protect_sensors(hass: HomeAssistant, entity_id: str) -> dict[str, str]:
    """The camera's Protect motion and detection sensors and doorbell event, with what each
    reports."""
    registry = er.async_get(hass)
    camera = registry.async_get(entity_id)
    if camera is None or camera.device_id is None:
        return {}
    camera_object_id = entity_id.split(".", 1)[1]
    sensors = {}
    for entry in er.async_entries_for_device(registry, camera.device_id):
        if entry.platform != PROTECT or entry.domain not in ("binary_sensor", "event"):
            continue
        kind = sensor_kind(_sensor_key(entry, camera_object_id))
        if kind is not None and (entry.domain == "binary_sensor" or kind == "doorbell"):
            sensors[entry.entity_id] = kind
    return dict(sorted(sensors.items()))


def event_spans(
    history: dict[str, list[Any]], kinds: dict[str, str], start: datetime, end: datetime
) -> list[dict[str, Any]]:
    """Times each sensor was on (a doorbell event entity: each press), as Unix seconds."""
    events = []
    for entity_id, states in history.items():
        kind = kinds.get(entity_id)
        if kind is None:
            continue
        if entity_id.startswith("event."):
            # An event entity's state is the moment it fired.
            for state in states:
                fired = dt_util.parse_datetime(state.state)
                if fired is not None and start <= fired <= end:
                    moment = int(fired.timestamp())
                    events.append({"start": moment, "end": moment, "kind": kind})
            continue
        since = None
        for state in states:
            changed = max(state.last_changed, start)
            if state.state == "on" and since is None:
                since = changed
            elif state.state != "on" and since is not None:
                events.append(
                    {"start": int(since.timestamp()), "end": int(changed.timestamp()), "kind": kind}
                )
                since = None
        if since is not None:
            events.append(
                {"start": int(since.timestamp()), "end": int(end.timestamp()), "kind": kind}
            )
    events.sort(key=lambda event: (event["start"], event["kind"]))
    return events[-MAX_EVENT_COUNT:]


async def _sensor_history(
    hass: HomeAssistant, entity_ids: list[str], start: datetime, end: datetime
) -> dict[str, list[Any]]:
    """Home Assistant's recorded states of these entities between start and end."""
    if not entity_ids or "recorder" not in hass.config.components:
        return {}
    from homeassistant.components.recorder import get_instance, history

    return await get_instance(hass).async_add_executor_job(
        partial(
            history.get_significant_states,
            hass,
            start,
            end,
            entity_ids,
            include_start_time_state=True,
            significant_changes_only=False,
            no_attributes=True,
        )
    )


class HistoryView(HomeAssistantView):
    """What UniFi Protect recorded of a camera, for Luma's Cameras app.

    The TV holds no Home Assistant credentials: like the stream view, this checks the camera's own
    access token. `at` (Unix seconds) answers with a picture from that moment and `start`/`end`
    with that stretch of recorded video, both by redirecting to the UniFi Protect integration's
    views with a path Home Assistant signs for a minute. `events` with `start`/`end` answers when
    the camera's Protect sensors saw motion, people, vehicles or a doorbell press.
    """

    url = HISTORY_VIEW_URL
    name = "api:luma:history"
    requires_auth = False

    def __init__(self, hass: HomeAssistant) -> None:
        self.hass = hass

    async def get(self, request: web.Request, entity_id: str) -> web.StreamResponse:
        camera = _camera(self.hass, entity_id) if entity_id.startswith("camera.") else None
        if camera is None:
            return self.json_message("Camera not found", 404)
        if request.query.get("token") not in camera.access_tokens:
            return self.json_message("Invalid camera token", 403)
        nvr = protect_entry(self.hass, entity_id)
        if nvr is None:
            return self.json_message("UniFi Protect does not record this camera", 404)
        query = request.query
        if "at" in query:
            moment = _moment(query["at"])
            if moment is None:
                return self.json_message("Invalid at", 400)
            path = PROTECT_SNAPSHOT.format(nvr=nvr, camera=entity_id, timestamp=_iso(moment))
            extra = {key: query[key] for key in ("width", "height") if query.get(key, "").isdigit()}
            return self._redirect(request, path, extra)
        start, end = _moment(query.get("start")), _moment(query.get("end"))
        if start is None or end is None or end <= start:
            return self.json_message("Invalid start or end", 400)
        if "events" in query:
            if end - start > MAX_EVENTS:
                return self.json_message("At most 48 hours of events at once", 400)
            sensors = protect_sensors(self.hass, entity_id)
            history = await _sensor_history(self.hass, list(sensors), start, end)
            return self.json({"events": event_spans(history, sensors, start, end)})
        if end - start > MAX_VIDEO:
            return self.json_message("At most 15 minutes of video at once", 400)
        path = PROTECT_VIDEO.format(nvr=nvr, camera=entity_id, start=_iso(start), end=_iso(end))
        return self._redirect(request, path, {})

    def _redirect(self, request: web.Request, path: str, extra: dict[str, str]) -> web.Response:
        signed = async_sign_path(self.hass, path, SIGNED_FOR, use_content_user=True)
        # Width and height are left out of the signature, so the TV may choose them.
        for key, value in extra.items():
            signed += f"&{key}={value}"
        location = f"{request.url.origin()}{signed}"
        return web.Response(status=302, headers={"Location": location, "Cache-Control": "no-store"})


class StreamView(HomeAssistantView):
    """Starts a camera's HLS stream for the TV and answers with its address.

    The TV holds no Home Assistant credentials, so this view checks the camera's own short-lived
    access token, exactly as Home Assistant's camera proxy does. The HLS address it returns carries
    the stream's own token, and the stream stops by itself once the TV stops loading it.
    """

    url = HLS_VIEW_URL
    name = "api:luma:stream"
    requires_auth = False

    def __init__(self, hass: HomeAssistant) -> None:
        self.hass = hass

    async def get(self, request: web.Request, entity_id: str) -> web.Response:
        camera = _camera(self.hass, entity_id) if entity_id.startswith("camera.") else None
        if camera is None:
            return self.json_message("Camera not found", 404)
        if request.query.get("token") not in camera.access_tokens:
            return self.json_message("Invalid camera token", 403)
        try:
            url = await _request_stream(self.hass, entity_id)
        except HomeAssistantError as err:
            return self.json_message(str(err), 404)
        return self.json({"url": url})


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    if hass.http is not None:
        hass.http.register_view(ExampleImageView())
        hass.http.register_view(StreamView(hass))
        hass.http.register_view(HistoryView(hass))

    async def show_camera(call: ServiceCall) -> None:
        data = dict(call.data)
        payload = overlay_payload(hass, data)
        if hls := await card_hls_url(hass, data):
            payload["hls_url"] = hls
        await _each(hass, _targets(hass, call), lambda client: client.show(payload))

    async def dismiss(call: ServiceCall) -> None:
        await _each(hass, _targets(hass, call), lambda client: client.dismiss())

    async def notify(call: ServiceCall) -> None:
        message = call.data["message"]
        await _each(hass, _targets(hass, call), lambda client: client.notify(message))

    hass.services.async_register(DOMAIN, SERVICE_SHOW_CAMERA, show_camera, SHOW_CAMERA_SCHEMA)
    hass.services.async_register(DOMAIN, SERVICE_DISMISS, dismiss, DISMISS_SCHEMA)
    hass.services.async_register(DOMAIN, SERVICE_NOTIFY, notify, NOTIFY_SCHEMA)
    return True
