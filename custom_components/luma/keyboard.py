"""Type on a phone: when a text field opens on the TV, the chosen phones get a notification
with a reply box, and what is typed there goes into the field.

The TV calls this entry's webhook (local network only) with `{"open": true, "field": ...}` or
`{"open": false}`. Phones are Home Assistant app notify services (`notify.mobile_app_*`); their
reply comes back as a `mobile_app_notification_action` event.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import TYPE_CHECKING, Any

from aiohttp import web
from homeassistant.components import webhook
from homeassistant.core import Event, HomeAssistant, callback
from homeassistant.exceptions import HomeAssistantError
from homeassistant.util import slugify

from .api import LumaApiError, LumaClient, LumaError
from .const import CONF_KEYBOARD_PHONES, CONF_WEBHOOK_ID, DOMAIN

if TYPE_CHECKING:
    from . import LumaConfigEntry

_LOGGER = logging.getLogger(__name__)
NOTIFY = "notify"
PHONE_PREFIX = "mobile_app_"
ACTION_EVENT = "mobile_app_notification_action"
# What the notification asks for, by the TV's field key.
PROMPTS = {
    "search": "Search",
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


def notification(entry: LumaConfigEntry, field: str) -> dict[str, Any]:
    prompt = PROMPTS.get(field, "Text")
    return {
        "title": entry.title,
        "message": f"Type here instead of with the remote: {prompt.lower()}.",
        "data": {
            "tag": tag(entry),
            "group": DOMAIN,
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
        # A Luma from before phone typing answers 404: leave it alone.
        _LOGGER.debug("Luma at %s cannot report text fields: %s", client.host, err)
        return
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
            await _notify(hass, entry, notification(entry, str(body.get("field") or "")))
        else:
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
        # A reply can hold line breaks (Enter on Android, pasted text); fields are one line.
        text = " ".join(str(event.data.get("reply_text") or "").split())
        if not text:
            return
        try:
            await client.type_text(text[:120])
        except LumaApiError as err:
            # 409: the field closed before the reply arrived.
            _LOGGER.debug("Luma at %s did not take the typed text: %s", client.host, err)
        except LumaError as err:
            _LOGGER.warning("Could not send typed text to %s: %s", client.host, err)

    @callback
    def ours(event_data: dict[str, Any]) -> bool:
        return event_data.get("action") == action(entry)

    entry.async_on_unload(hass.bus.async_listen(ACTION_EVENT, replied, ours))
