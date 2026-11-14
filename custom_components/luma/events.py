"""What the TV reports about itself: Luma's log and performance, live.

Each loaded TV keeps `GET /api/events` open. Luma's warnings and errors go to Home Assistant's
log (logger `custom_components.luma.tv`), every log entry fires a `luma_event` event, and the
latest performance summary feeds the TV's diagnostic sensors. A history of both survives
restarts and is part of the TV's diagnostics download.

Every entry carries Luma's cursor (`boot:seq`). Reconnecting with the last one gets exactly what
was missed while Home Assistant or the network was away, as far as the TV still holds it.
"""

from __future__ import annotations

import asyncio
import logging
from collections import deque
from datetime import UTC, datetime
from typing import Any

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers.dispatcher import async_dispatcher_send
from homeassistant.helpers.storage import Store

from .api import LumaAuthError, LumaClient, LumaError
from .const import CONF_DEVICE_ID, DOMAIN

_LOGGER = logging.getLogger(__name__)
# Luma's own entries, so they can be filtered or raised to debug on their own.
TV_LOGGER = logging.getLogger(f"{__package__}.tv")

EVENT_LUMA = "luma_event"
STORAGE_VERSION = 1
# The history kept: log entries, and performance summaries (one a minute while Luma draws).
MAX_LOGS = 1000
MAX_METRICS = 720
SAVE_DELAY = 300
# Reconnecting: from a few seconds after a drop up to five minutes while the TV stays off.
RETRY_FIRST = 5
RETRY_MAX = 300
LEVELS = {"info": logging.INFO, "warning": logging.WARNING, "error": logging.ERROR}


def signal(entry_id: str) -> str:
    """Dispatched whenever something the sensors show changed."""
    return f"{DOMAIN}_events_{entry_id}"


def store(hass: HomeAssistant, entry_id: str) -> Store[dict[str, Any]]:
    """The saved cursor and history of one TV's config entry."""
    return Store(hass, STORAGE_VERSION, f"{DOMAIN}.events.{entry_id}")


def entry_time(entry: dict[str, Any]) -> str | None:
    """An entry's Unix milliseconds as an ISO time."""
    try:
        return datetime.fromtimestamp(int(entry["time"]) / 1000, UTC).isoformat()
    except (KeyError, TypeError, ValueError, OverflowError, OSError):
        return None


class LumaEvents:
    """Follows one TV's event stream for as long as its config entry is loaded."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry, client: LumaClient) -> None:
        self.hass = hass
        self.entry = entry
        self.client = client
        self.logs: deque[dict[str, Any]] = deque(maxlen=MAX_LOGS)
        self.metrics: deque[dict[str, Any]] = deque(maxlen=MAX_METRICS)
        self.cursor: str | None = None
        self.connected = False
        self.luma: dict[str, Any] = {}
        self.last_problem: dict[str, Any] | None = None
        self._store = store(hass, entry.entry_id)

    async def async_start(self) -> None:
        stored = await self._store.async_load() or {}
        self.cursor = stored.get("cursor")
        self.logs.extend(entry for entry in stored.get("logs", []) if isinstance(entry, dict))
        self.metrics.extend(entry for entry in stored.get("metrics", []) if isinstance(entry, dict))
        self.last_problem = next(
            (entry for entry in reversed(self.logs) if entry.get("level") != "info"), None
        )
        # The sensors were added before the history loaded: show the last problem now.
        self._changed()
        self.entry.async_create_background_task(
            self.hass, self._follow(), f"{DOMAIN} events {self.entry.entry_id}"
        )
        self.entry.async_on_unload(self.async_save)

    async def async_save(self) -> None:
        await self._store.async_save(self._data())

    @callback
    def _data(self) -> dict[str, Any]:
        return {"cursor": self.cursor, "logs": list(self.logs), "metrics": list(self.metrics)}

    def latest(self, key: str) -> Any:
        """A value from the latest performance summary."""
        return self.metrics[-1].get("data", {}).get(key) if self.metrics else None

    def diagnostics(self) -> dict[str, Any]:
        return {
            "connected": self.connected,
            "cursor": self.cursor,
            "luma": self.luma,
            "last_problem": self.last_problem,
            "logs": list(self.logs),
            "metrics": list(self.metrics),
        }

    async def _follow(self) -> None:
        delay = RETRY_FIRST
        while True:
            try:
                async for event, event_id, data in self.client.events(self.cursor):
                    if event == "hello":
                        delay = RETRY_FIRST
                    self._handle(event, event_id, data)
                # Luma closed the stream: it stopped, or this server's pairing changed.
            except LumaAuthError:
                self.entry.async_start_reauth(self.hass)
                delay = RETRY_MAX
            except LumaError as err:
                _LOGGER.debug("No events from %s: %s", self.entry.title, err)
            if self.connected:
                self.connected = False
                self._changed()
            await asyncio.sleep(delay)
            delay = min(delay * 2, RETRY_MAX)

    @callback
    def _handle(self, event: str, event_id: str | None, data: dict[str, Any]) -> None:
        title = self.entry.title
        if event == "hello":
            if data.get("restarted"):
                TV_LOGGER.info("%s: Luma restarted", title)
            self.luma = data
            self.connected = True
            self._changed()
            return
        if event == "missed":
            TV_LOGGER.warning(
                "%s: Luma recorded more than it could keep while Home Assistant was away; "
                "some entries were lost",
                title,
            )
            return
        if event_id:
            self.cursor = event_id
        if event == "metrics":
            self.metrics.append(data)
            self._changed()
        elif event == "log":
            self.logs.append(data)
            level = str(data.get("level", "info"))
            area = str(data.get("area", ""))
            message = str(data.get("message", ""))
            TV_LOGGER.log(LEVELS.get(level, logging.INFO), "%s [%s] %s", title, area, message)
            luma_id = self.entry.data.get(CONF_DEVICE_ID)
            device = dr.async_get(self.hass).async_get_device(identifiers={(DOMAIN, luma_id)})
            self.hass.bus.async_fire(
                EVENT_LUMA,
                {
                    "config_entry_id": self.entry.entry_id,
                    # Home Assistant's device, as device triggers and device_attr() expect.
                    "device_id": device.id if device else None,
                    "level": level,
                    "area": area,
                    "message": message,
                    "time": entry_time(data),
                },
            )
            if level != "info":
                self.last_problem = data
                self._changed()
        else:
            return
        self._store.async_delay_save(self._data, SAVE_DELAY)

    @callback
    def _changed(self) -> None:
        async_dispatcher_send(self.hass, signal(self.entry.entry_id))
