"""The TV's diagnostics download: what Luma logged and how it performed, as one JSON file."""

from __future__ import annotations

from typing import Any

from homeassistant.components.diagnostics import async_redact_data
from homeassistant.const import CONF_WEBHOOK_ID
from homeassistant.core import HomeAssistant

from . import LumaConfigEntry
from .api import LumaError
from .const import (
    CONF_CRITERION_CLEARED,
    CONF_CRITERION_EARLIER,
    CONF_CRITERION_EMAIL,
    CONF_CRITERION_FINGERPRINT,
    CONF_CRITERION_PASSWORD,
    CONF_DEV_CF_ID,
    CONF_DEV_CF_SECRET,
    CONF_DEV_KEY,
    CONF_DEV_TOKEN_HASH,
    CONF_MDBLIST_KEY,
    CONF_MOTN_KEY,
    CONF_TMDB_TOKEN,
    CONF_TOKEN,
)

# The keyboard webhook's ID is its only secret: anyone who has it can call it.
REDACT = {
    CONF_TOKEN,
    CONF_MOTN_KEY,
    CONF_TMDB_TOKEN,
    CONF_MDBLIST_KEY,
    CONF_WEBHOOK_ID,
    CONF_DEV_KEY,
    CONF_DEV_TOKEN_HASH,
    CONF_DEV_CF_ID,
    CONF_DEV_CF_SECRET,
    CONF_CRITERION_EMAIL,
    CONF_CRITERION_PASSWORD,
    CONF_CRITERION_CLEARED,
    CONF_CRITERION_FINGERPRINT,
    CONF_CRITERION_EARLIER,
}


async def async_get_config_entry_diagnostics(
    hass: HomeAssistant, entry: LumaConfigEntry
) -> dict[str, Any]:
    data = entry.runtime_data
    # What the TV holds right now, when it is on: its own log can reach further back than the
    # stream did, for example to before this server paired.
    try:
        tv: dict[str, Any] = await data.client.diagnostics()
    except LumaError as err:
        tv = {"error": str(err)}
    return {
        "entry": {
            "title": entry.title,
            "data": async_redact_data(dict(entry.data), REDACT),
            "options": async_redact_data(dict(entry.options), REDACT),
        },
        "stream": data.events.diagnostics(),
        "tv": tv,
    }
