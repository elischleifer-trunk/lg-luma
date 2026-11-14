"""Type on a phone: when a text field opens on the TV, the chosen phones get a notification
with a reply box, and what is typed there goes into the field.

The TV calls this entry's webhook (local network only) with `{"open": true, "field": ...}` or
`{"open": false}`. Phones are Home Assistant app notify services (`notify.mobile_app_*`); their
reply comes back as a `mobile_app_notification_action` event.

Tapping the notification, rather than replying to it, opens the Home Assistant app on a page with
one text box (`keyboard-panel.js`, a panel that is not in the sidebar), which sends what is typed
over the app's own connection.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

import voluptuous as vol
from aiohttp import web
from homeassistant.components import webhook, websocket_api
from homeassistant.components.http import HomeAssistantView
from homeassistant.config_entries import ConfigEntryState
from homeassistant.core import Event, HomeAssistant, callback
from homeassistant.exceptions import HomeAssistantError
from homeassistant.loader import async_get_integration
from homeassistant.util import slugify

from .api import LumaApiError, LumaAuthError, LumaClient, LumaError
from .const import CONF_KEYBOARD_PHONES, CONF_WEBHOOK_ID, DOMAIN

if TYPE_CHECKING:
    from . import LumaConfigEntry

_LOGGER = logging.getLogger(__name__)
NOTIFY = "notify"
PHONE_PREFIX = "mobile_app_"
ACTION_EVENT = "mobile_app_notification_action"
# The page a tapped notification opens, and the script Home Assistant's frontend loads for it.
PANEL_PATH = "luma-keyboard"
PANEL_ELEMENT = "luma-keyboard-panel"
PANEL_SCRIPT_URL = "/api/luma/keyboard-panel.js"
PANEL_SCRIPT = Path(__file__).parent / "keyboard-panel.js"
MAX_TEXT = 120
# What the notification asks for, by the TV's field key.
PROMPTS = {
    "search": "Search",
    "titles": "Movies & shows",
    "weather_location": "Weather location",
    "greeting_name": "Your name",
    "input_name": "Input name",
    "camera_name": "Camera name",
}


def available_phones(hass: HomeAssistant) -> dict[str, str]:
    """Home Assistant app notify services and the phone names they belong to."""
    names = {}
    for entry in hass.config_entries.async_entries("mobile_app"):
        name = entry.data.get("device_name") or entry.title
        names[f"{PHONE_PREFIX}{slugify(name)}"] = name
    return {
        service: names.get(service, service.removeprefix(PHONE_PREFIX).replace("_", " "))
        for service in sorted(hass.services.async_services_for_domain(NOTIFY))
        if service.startswith(PHONE_PREFIX)
    }


def phones(entry: LumaConfigEntry) -> list[str]:
    return list(entry.options.get(CONF_KEYBOARD_PHONES) or [])


def tag(entry: LumaConfigEntry) -> str:
    """Replaces the previous notification on the phone and lets it be cleared."""
    return f"luma_keyboard_{entry.entry_id}"


def action(entry: LumaConfigEntry) -> str:
    return f"LUMA_KEYBOARD_{entry.entry_id}"


def panel_url(entry: LumaConfigEntry) -> str:
    """The typing page for this TV, as a path inside the Home Assistant app."""
    return f"/{PANEL_PATH}?entry={entry.entry_id}"


def clean(text: Any) -> str:
    """One line, as the TV's fields are: line breaks (Enter on Android, pasted text) become
    spaces."""
    return " ".join(str(text or "").split())[:MAX_TEXT]


def notification(entry: LumaConfigEntry, field: str) -> dict[str, Any]:
    prompt = PROMPTS.get(field, "Text")
    return {
        "title": entry.title,
        "message": f"Tap to type on your phone instead of with the remote: {prompt.lower()}.",
        "data": {
            "tag": tag(entry),
            "group": DOMAIN,
            # A tap opens the typing page: `url` on iPhone, `clickAction` on Android.
            "url": panel_url(entry),
            "clickAction": panel_url(entry),
            # Android: delivered at once, in a channel the owner can silence on its own.
            "ttl": 0,
            "priority": "high",
            "channel": "Luma keyboard",
            "actions": [
                {
                    "action": action(entry),
                    "title": "Type",
                    "behavior": "textInput",
                    "textInputButtonTitle": "Send",
                    "textInputPlaceholder": prompt,
                }
            ],
        },
    }


async def _notify(hass: HomeAssistant, entry: LumaConfigEntry, message: dict[str, Any]) -> None:
    for service in phones(entry):
        if not hass.services.has_service(NOTIFY, service):
            _LOGGER.debug("Phone %s has no notify service any more", service)
            continue
        try:
            await hass.services.async_call(NOTIFY, service, message, blocking=True)
        except HomeAssistantError as err:
            # One phone failing must not stop the others.
            _LOGGER.warning("Could not notify %s: %s", service, err)


async def register(
    hass: HomeAssistant,
    entry: LumaConfigEntry,
    client: LumaClient,
    base_url: Callable[[], str],
) -> None:
    """Tell the TV where to report its text fields, or that it need not; best effort. A field
    that is open already is reported to the new webhook straight away."""
    url = None
    if phones(entry):
        try:
            url = base_url() + webhook.async_generate_path(entry.data[CONF_WEBHOOK_ID])
        except HomeAssistantError as err:
            _LOGGER.warning("The TV cannot reach Home Assistant to ask for typing: %s", err)
            return
    try:
        await client.keyboard_hook(url)
        entry.runtime_data.keyboard = url is not None
    except LumaApiError as err:
        # A Luma from before phone typing answers 404: Repairs asks for a Luma update, but only
        # when phone typing was turned on, so removing a webhook never raises it.
        if err.status == 404 and url:
            entry.runtime_data.compatibility.missing_endpoint()
        _LOGGER.debug("Luma at %s cannot report text fields: %s", client.host, err)
    except LumaError as err:
        _LOGGER.debug("Could not register the keyboard webhook with %s: %s", client.host, err)


def needs_register(entry: LumaConfigEntry, status: dict[str, Any]) -> bool:
    """Whether the TV lost the webhook (it restarted) or still has one no phone wants."""
    held = status.get("keyboard_hook")
    if not isinstance(held, bool):
        return False  # A Luma from before phone typing.
    return held != bool(phones(entry))


@callback
def setup(hass: HomeAssistant, entry: LumaConfigEntry, client: LumaClient) -> None:
    """Listen for the TV's text fields and the phones' replies while the entry is loaded."""

    async def handle(hass: HomeAssistant, webhook_id: str, request: web.Request) -> None:
        try:
            body = await request.json()
        except ValueError:
            return
        if not isinstance(body, dict) or not phones(entry):
            return
        if body.get("open"):
            field = str(body.get("field") or "")
            entry.runtime_data.field = field
            await _notify(hass, entry, notification(entry, field))
        else:
            entry.runtime_data.field = None
            await _notify(
                hass, entry, {"message": "clear_notification", "data": {"tag": tag(entry)}}
            )

    webhook_id = entry.data[CONF_WEBHOOK_ID]
    webhook.async_register(
        hass,
        DOMAIN,
        f"{entry.title} keyboard",
        webhook_id,
        handle,
        local_only=True,
        allowed_methods=["POST"],
    )
    entry.async_on_unload(lambda: webhook.async_unregister(hass, webhook_id))

    async def replied(event: Event) -> None:
        text = clean(event.data.get("reply_text"))
        if not text:
            return
        try:
            await client.type_text(text)
        except LumaApiError as err:
            # 409: the field closed before the reply arrived.
            _LOGGER.debug("Luma at %s did not take the typed text: %s", client.host, err)
        except LumaError as err:
            _LOGGER.warning("Could not send typed text to %s: %s", client.host, err)

    @callback
    def ours(event_data: dict[str, Any]) -> bool:
        return event_data.get("action") == action(entry)

    entry.async_on_unload(hass.bus.async_listen(ACTION_EVENT, replied, ours))


class PanelScriptView(HomeAssistantView):
    """Serves the typing page's script, which the frontend loads without credentials."""

    url = PANEL_SCRIPT_URL
    name = "api:luma:keyboard-panel"
    requires_auth = False

    async def get(self, request: web.Request) -> web.FileResponse:
        return web.FileResponse(PANEL_SCRIPT, headers={"Cache-Control": "max-age=86400"})


def _loaded(hass: HomeAssistant) -> list[LumaConfigEntry]:
    return [
        entry
        for entry in hass.config_entries.async_entries(DOMAIN)
        if entry.state is ConfigEntryState.LOADED
    ]


@websocket_api.websocket_command({vol.Required("type"): "luma/keyboard/tvs"})
@callback
def ws_tvs(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Every TV, with what its open text field asks for (None: none open, or not reported
    because no phone is chosen)."""
    tvs = []
    for entry in _loaded(hass):
        field = entry.runtime_data.field
        tvs.append(
            {
                "entry_id": entry.entry_id,
                "title": entry.title,
                "prompt": None if field is None else PROMPTS.get(field, "Text"),
            }
        )
    connection.send_result(msg["id"], {"tvs": tvs})


@websocket_api.websocket_command(
    {
        vol.Required("type"): "luma/keyboard/type",
        vol.Required("entry_id"): str,
        vol.Required("text"): str,
    }
)
@websocket_api.async_response
async def ws_type(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Type into a TV's open text field from the typing page."""
    entry = next((entry for entry in _loaded(hass) if entry.entry_id == msg["entry_id"]), None)
    if entry is None:
        connection.send_error(msg["id"], websocket_api.ERR_NOT_FOUND, "This TV is not set up")
        return
    text = clean(msg["text"])
    if not text:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, "Nothing to type")
        return
    try:
        await entry.runtime_data.client.type_text(text)
    except LumaAuthError:
        entry.async_start_reauth(hass)
        connection.send_error(msg["id"], "pair_again", "Pair this TV with Home Assistant again")
    except LumaApiError as err:
        if err.status == 409:
            connection.send_error(msg["id"], "no_field", "No text field is open on the TV")
        else:
            connection.send_error(msg["id"], "tv_error", str(err))
    except LumaError as err:
        connection.send_error(msg["id"], "tv_unreachable", str(err))
    else:
        connection.send_result(msg["id"])


async def async_setup_panel(hass: HomeAssistant) -> None:
    """The page a tapped notification opens. Without Home Assistant's frontend there is no
    app to open it in, and the notification's reply box still works."""
    if hass.http is None or "frontend" not in hass.config.components:
        return
    from homeassistant.components import frontend

    hass.http.register_view(PanelScriptView())
    websocket_api.async_register_command(hass, ws_tvs)
    websocket_api.async_register_command(hass, ws_type)
    # The version makes the app load the new script after an update.
    version = (await async_get_integration(hass, DOMAIN)).version
    frontend.async_register_built_in_panel(
        hass,
        "custom",
        frontend_url_path=PANEL_PATH,
        config={
            "_panel_custom": {
                "name": PANEL_ELEMENT,
                "embed_iframe": False,
                "trust_external": False,
                "module_url": f"{PANEL_SCRIPT_URL}?v={version}",
            }
        },
        require_admin=False,
    )
