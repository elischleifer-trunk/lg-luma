"""Streaming search keys: checking them with their services and keeping each TV in step.

Luma's search on the TV finds where shows stream with Movie of the Night, ranks them with TMDB
and shows review scores from MDBList. The owner enters the keys in this integration's options;
Home Assistant checks them, then pushes them to every paired TV, and again whenever a TV lost
them (a reset) or had them changed on its own QR page. A key left empty here is never sent,
so it never erases one.
"""

from __future__ import annotations

import logging
from typing import Any

import aiohttp
from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession

from .api import LumaClient, LumaError
from .const import CONF_MDBLIST_KEY, CONF_MOTN_KEY, CONF_TMDB_TOKEN

_LOGGER = logging.getLogger(__name__)
TIMEOUT = aiohttp.ClientTimeout(total=10)
MOTN_URL = "https://api.movieofthenight.com/v4/genres"
MOTN_RAPIDAPI_HOST = "streaming-availability.p.rapidapi.com"
# Ghostbusters (1984): any title works, this one is certain to exist on TMDB.
TMDB_URL = "https://api.themoviedb.org/3/find/tt0087332?external_source=imdb_id"
# The account endpoint checks a key without spending a lookup.
MDBLIST_URL = "https://api.mdblist.com/user"


def keys(options: dict[str, Any]) -> tuple[str, str, str]:
    """Movie of the Night key, TMDB token and MDBList key, trimmed; empty when not set."""
    return tuple(  # type: ignore[return-value]
        (options.get(name) or "").strip()
        for name in (CONF_MOTN_KEY, CONF_TMDB_TOKEN, CONF_MDBLIST_KEY)
    )


def _motn_request(key: str) -> tuple[str, dict[str, str]]:
    """Developers Platform keys go to Movie of the Night, other keys to RapidAPI."""
    if key.startswith("motn-key-"):
        return MOTN_URL, {"X-API-Key": key}
    return (
        f"https://{MOTN_RAPIDAPI_HOST}/genres",
        {"X-RapidAPI-Key": key, "X-RapidAPI-Host": MOTN_RAPIDAPI_HOST},
    )


async def _refused(
    hass: HomeAssistant,
    url: str,
    headers: dict[str, str],
    params: dict[str, str] | None = None,
) -> bool:
    """Whether the service refused the key. An unreachable service does not count: the key
    is kept unchecked rather than blocking the owner while a service is down."""
    try:
        async with async_get_clientsession(hass).get(
            url, headers=headers, params=params, timeout=TIMEOUT
        ) as response:
            return response.status in (401, 403)
    except (aiohttp.ClientError, TimeoutError) as err:
        _LOGGER.debug("Could not check a streaming key with %s: %s", url, err)
        return False


async def check(hass: HomeAssistant, motn: str, tmdb: str, mdblist: str = "") -> dict[str, str]:
    """Form errors for keys their service refuses; empty keys are not checked."""
    errors = {}
    if motn and await _refused(hass, *_motn_request(motn)):
        errors[CONF_MOTN_KEY] = "motn_refused"
    if tmdb and await _refused(hass, TMDB_URL, {"Authorization": f"Bearer {tmdb}"}):
        errors[CONF_TMDB_TOKEN] = "tmdb_refused"
    # MDBList takes its key as a query parameter.
    if mdblist and await _refused(hass, MDBLIST_URL, {}, {"apikey": mdblist}):
        errors[CONF_MDBLIST_KEY] = "mdblist_refused"
    return errors


def needs_push(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether a TV's status shows it lacks a key Home Assistant holds, or its keys were last
    set on the TV itself. TVs without streaming search report nothing and are left alone."""
    held = status.get("streaming_keys")
    if not isinstance(held, dict):
        return False
    motn, tmdb, mdblist = keys(options)
    # An older Luma does not report MDBList and cannot take that key, so it is nothing to push.
    if "mdblist" not in held:
        mdblist = ""
    if not (motn or tmdb or mdblist):
        return False
    return (
        (bool(motn) and not held.get("motn"))
        or (bool(tmdb) and not held.get("tmdb"))
        or (bool(mdblist) and not held.get("mdblist"))
        or not held.get("home_assistant")
    )


async def push(client: LumaClient, options: dict[str, Any]) -> None:
    """Send the keys held here; best effort, the TV may be off."""
    motn, tmdb, mdblist = keys(options)
    if not (motn or tmdb or mdblist):
        return
    try:
        await client.streaming_keys(motn, tmdb, mdblist)
    except LumaError as err:
        _LOGGER.debug("Could not send streaming keys to %s: %s", client.host, err)
