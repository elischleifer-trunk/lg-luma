"""A backup of the TV's Criterion Channel sign-in, so a reset does not sign the player out.

Once the TV's Criterion player has signed in, Luma keeps the Channel's web session: a few
browser cookies, the one that matters being `__session` on www.criterionchannel.com. It lasts
about 30 days and the TV renews it in the background. The TV reports in its status when the
session it holds expires, when it last confirmed it works, and whether it found it signed out;
older Luma versions report nothing, and are left alone.

Home Assistant keeps a private copy, one per config entry, in its own storage (never in the
options, the logs, diagnostics or entity attributes). At each status check:

- When the TV holds a working session that expires later than the copy (or there is no copy),
  Home Assistant fetches it and replaces the copy.
- When the copy is still good and the TV holds none (a reset), an older one, or one it found
  signed out, Home Assistant offers the copy. The TV keeps it only if it expires later than its
  own; a copy the TV refused is not offered again, until a newer one replaces it.
- When the TV found its session signed out and has nothing newer to take, the owner gets one
  notification per session asking them to sign in again in the Criterion player.

Clearing the Criterion account in the options also forgets the session: the copy goes, and the
TV is told to drop its own. If the TV is off then, that waits for the next status check.

The TV holds one sign-in, and with several servers paired only one may look after it: the server
whose account the TV holds (by fingerprint, as for the account itself), or, when the TV holds no
account and this server has none set, any server (a sign-in typed in the player). A server whose
account the TV does not hold leaves the sign-in alone, and drops its copy once the TV holds
another server's account, so it never restores or removes a sign-in that is not its own.
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from homeassistant.components import persistent_notification
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_send
from homeassistant.helpers.storage import Store

from . import criterion_account
from .api import LumaApiError, LumaClient, LumaError
from .const import CONF_CRITERION_CLEARED, CONF_CRITERION_EARLIER, DOMAIN

if TYPE_CHECKING:
    from homeassistant.config_entries import ConfigEntry

_LOGGER = logging.getLogger(__name__)
STORAGE_VERSION = 1
# The same limits the TV applies.
MAX_COOKIES = 64
MAX_NAME = 256
MAX_VALUE = 8192
DOMAINS = ("criterionchannel.com", "criterion.com")
SESSION_COOKIE = "__session"
# Counted as the TV counts them: either host, written with or without a leading dot.
SESSION_HOSTS = ("www.criterionchannel.com", "criterionchannel.com")
# The copy saved while a clearing waits for the TV to drop its session.
FORGET = {"forget": True}


def signal(entry_id: str) -> str:
    """Dispatched whenever the TV reports its sign-in."""
    return f"{DOMAIN}_criterion_session_{entry_id}"


def store(hass: HomeAssistant, entry_id: str) -> Store[dict[str, Any]]:
    """The private copy of one TV's sign-in: `{"cookies": [...], "expires": ...}`."""
    return Store(hass, STORAGE_VERSION, f"{DOMAIN}.criterion_session.{entry_id}", private=True)


def _number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    return float(value)


def _criterion_domain(domain: str) -> bool:
    host = domain.lower().removeprefix(".")
    return any(host == name or host.endswith(f".{name}") for name in DOMAINS)


def expiry(cookies: Any) -> float | None:
    """When a cookie list's sign-in expires (its `__session` cookie's expiry), or None unless
    it is a list the TV would take."""
    if not isinstance(cookies, list) or not 0 < len(cookies) <= MAX_COOKIES:
        return None
    expires = None
    for cookie in cookies:
        if not isinstance(cookie, dict):
            return None
        name, value, domain = cookie.get("name"), cookie.get("value"), cookie.get("domain")
        if not (isinstance(name, str) and name and len(name) <= MAX_NAME):
            return None
        if not (isinstance(value, str) and len(value) <= MAX_VALUE):
            return None
        if not (isinstance(domain, str) and _criterion_domain(domain)):
            return None
        if name == SESSION_COOKIE and value and domain.lower().lstrip(".") in SESSION_HOSTS:
            moment = _number(cookie.get("expires"))
            if moment is not None and (expires is None or moment > expires):
                expires = moment
    return expires


@dataclass
class State:
    """What one entry knows of the sign-in between status checks, in memory."""

    # Whether the TV's status reports a sign-in at all (a Luma that keeps one).
    known: bool = False
    # The TV's latest `criterion_session` status: None when it holds none.
    reported: dict[str, Any] | None = None
    # The expiry of the copy the TV last refused, not offered again.
    refused: float | None = None
    # The expiry of the signed-out session the owner was last told about, in a tuple.
    notified: tuple[float | None] | None = None
    # Whether the Criterion account was set in the options, to notice it being cleared.
    account: bool = False
    # The TV's latest `criterion_account` status: the fingerprint of the account it holds, True
    # for one sent without, or False for none.
    holder: Any = False
    _copy: dict[str, Any] | None = None
    _loaded: bool = False

    @property
    def expires(self) -> datetime | None:
        """When the TV's sign-in expires, for its sensor."""
        moment = _number((self.reported or {}).get("expires"))
        if moment is None:
            return None
        try:
            return datetime.fromtimestamp(moment, UTC)
        except (OverflowError, OSError, ValueError):
            return None

    async def copy(self, hass: HomeAssistant, entry_id: str) -> dict[str, Any] | None:
        if not self._loaded:
            stored = await store(hass, entry_id).async_load()
            self._copy = stored if isinstance(stored, dict) else None
            self._loaded = True
        return self._copy

    async def save(self, hass: HomeAssistant, entry_id: str, copy: dict[str, Any] | None) -> None:
        if copy is None:
            await store(hass, entry_id).async_remove()
        else:
            await store(hass, entry_id).async_save(copy)
        self._copy = copy
        self._loaded = True


def _state(entry: ConfigEntry) -> State:
    return entry.runtime_data.criterion_sign_in


def _held(copy: dict[str, Any] | None) -> float | None:
    """The copy's expiry, when it holds a sign-in."""
    if not copy or copy.get("forget"):
        return None
    return _number(copy.get("expires"))


def _ours(options: dict[str, Any], holder: Any) -> bool:
    """Whether this server looks after the TV's sign-in: the TV holds this server's account,
    or neither holds one."""
    mark = criterion_account.fingerprint(options)
    return holder == mark if mark is not None else holder is False


def _another(options: dict[str, Any], holder: Any) -> bool:
    """Whether the TV holds an account another server sent."""
    sent = [
        criterion_account.fingerprint(options),
        options.get(CONF_CRITERION_CLEARED),
        *options.get(CONF_CRITERION_EARLIER, []),
    ]
    return isinstance(holder, str) and holder not in sent


@callback
def _reported(hass: HomeAssistant, entry: ConfigEntry, state: State, value: Any) -> None:
    state.known = True
    state.reported = value if isinstance(value, dict) else None
    async_dispatcher_send(hass, signal(entry.entry_id))


async def sync(
    hass: HomeAssistant,
    entry: ConfigEntry,
    client: LumaClient,
    status: dict[str, Any],
    *,
    sent: bool = False,
) -> None:
    """Back up the TV's sign-in, or give the TV the copy, after a status check. Best effort:
    the TV may be off, and the next status check tries again. `sent`: this check has just given
    the TV this server's account, which `status` (read before) does not show yet."""
    if "criterion_session" not in status:
        return
    state = _state(entry)
    mark = criterion_account.fingerprint(dict(entry.options))
    state.holder = mark if sent and mark else status.get("criterion_account", False)
    _reported(hass, entry, state, status["criterion_session"])
    reported = state.reported
    copy = await state.copy(hass, entry.entry_id)
    if copy and copy.get("forget"):
        await _forget_on_tv(hass, entry, client, state)
        return
    options = dict(entry.options)
    if not _ours(options, state.holder):
        if copy is not None and _another(options, state.holder):
            # Another server's account, and with it its sign-in: this copy is not to restore.
            await state.save(hass, entry.entry_id, None)
            state.refused = None
        _notice(hass, entry, state, restored=False, ours=False)
        return
    held = _held(copy)
    on_tv = _number(reported.get("expires")) if reported else None
    working = reported is not None and reported.get("valid") is not False
    restored = False
    try:
        if working and on_tv is not None and (held is None or on_tv > held):
            await _back_up(hass, entry, client, state, held)
        elif (
            held is not None
            and held > time.time()
            and held != state.refused
            and (reported is None or on_tv is None or on_tv < held or not working)
        ):
            restored = await _restore(hass, entry, client, state, copy, held)
    except LumaError as err:
        # The next check tries again; no notice about a sign-in that may yet be restored.
        _LOGGER.debug("Could not sync the Criterion sign-in with %s: %s", client.host, err)
        return
    _notice(hass, entry, state, restored)


async def _back_up(
    hass: HomeAssistant,
    entry: ConfigEntry,
    client: LumaClient,
    state: State,
    held: float | None,
) -> None:
    session = await client.criterion_session()
    if session is None:
        return
    cookies = session.get("cookies")
    expires = expiry(cookies)
    if expires is None:
        _LOGGER.debug("The Criterion sign-in from %s is not one to keep", client.host)
        return
    if held is not None and expires <= held:
        return
    await state.save(hass, entry.entry_id, {"cookies": cookies, "expires": expires})
    _LOGGER.debug("Backed up the Criterion sign-in from %s, expiring %s", client.host, expires)


async def _restore(
    hass: HomeAssistant,
    entry: ConfigEntry,
    client: LumaClient,
    state: State,
    copy: dict[str, Any],
    held: float,
) -> bool:
    """Offer the copy; whether the TV kept it."""
    try:
        reply = await client.set_criterion_session(copy.get("cookies") or [])
    except LumaApiError as err:
        if err.status == 400:
            # A list this TV will not take: offering it again would not change that.
            state.refused = held
        raise
    if "criterion_session" in reply:
        _reported(hass, entry, state, reply["criterion_session"])
    if reply.get("stored") is True:
        _LOGGER.debug("Gave %s the Criterion sign-in, expiring %s", client.host, held)
        return True
    state.refused = held
    _LOGGER.debug("%s kept its own Criterion sign-in over the copy", client.host)
    return False


@callback
def _notice(
    hass: HomeAssistant, entry: ConfigEntry, state: State, restored: bool, *, ours: bool = True
) -> None:
    """Tell the owner once per signed-out session, when nothing newer could replace it; never
    about a sign-in another server looks after."""
    reported = state.reported
    notice_id = f"{DOMAIN}_criterion_session_{entry.entry_id}"
    if not ours or reported is None or reported.get("valid") is not False or restored:
        if state.notified is not None:
            state.notified = None
            persistent_notification.async_dismiss(hass, notice_id)
        return
    expires = (_number(reported.get("expires")),)
    if state.notified == expires:
        return
    state.notified = expires
    persistent_notification.async_create(
        hass,
        f"The Criterion Channel sign-in on {entry.title} has ended, and Home Assistant has no "
        "newer one to give it. Open the Criterion Channel in Luma and sign in again; Home "
        "Assistant keeps a copy of the new sign-in.",
        title="Sign in to the Criterion Channel again",
        notification_id=notice_id,
    )


async def _forget_on_tv(
    hass: HomeAssistant, entry: ConfigEntry, client: LumaClient, state: State
) -> None:
    """Drop the TV's sign-in after the account was cleared, when it still holds an account this
    server sent; the copy is already gone."""
    holder = {"criterion_account": state.holder}
    if state.reported is not None and criterion_account.needs_forget(dict(entry.options), holder):
        try:
            reply = await client.forget_criterion_session()
        except LumaError as err:
            _LOGGER.debug("Could not remove the Criterion sign-in from %s: %s", client.host, err)
            return
        _reported(hass, entry, state, reply.get("criterion_session"))
    await state.save(hass, entry.entry_id, None)


async def forget(hass: HomeAssistant, entry: ConfigEntry, client: LumaClient) -> None:
    """The Criterion account was cleared: drop the copy, and the TV's sign-in when it reports
    one and still holds this server's account. A TV that is off, or has not reported yet, is
    asked at a later status check."""
    state = _state(entry)
    await state.save(hass, entry.entry_id, dict(FORGET))
    state.refused = None
    if state.known:
        await _forget_on_tv(hass, entry, client, state)


async def diagnostics(hass: HomeAssistant, entry: ConfigEntry) -> dict[str, Any]:
    """What the diagnostics download shows: expiries only, never a cookie."""
    state = _state(entry)
    copy = await state.copy(hass, entry.entry_id)
    reported = state.reported
    return {
        "tv": (
            {key: reported.get(key) for key in ("expires", "checked", "valid")}
            if reported
            else reported
        )
        if state.known
        else "not reported",
        "backup_expires": _held(copy),
        "forget_pending": bool(copy and copy.get("forget")),
    }
