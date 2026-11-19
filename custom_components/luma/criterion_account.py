"""The Criterion Channel account the TV's Criterion player signs in with.

The owner enters it in this integration's options. Home Assistant sends it to every paired TV
with a fingerprint: a hash keyed with this entry's ID, which says nothing about the password.
Each TV reports the fingerprint of the account it holds, so Home Assistant sends the account
again whenever the TV's differs: after a reset, or a change made while the TV was off.

Clearing both fields records the fingerprint of the account that was cleared. A TV still holding
that account is told to forget it; an account another Home Assistant server sent is left alone.
The TV hands the account only to its own Criterion player, on the Channel's sign-in page.
"""

from __future__ import annotations

import hashlib
import hmac
import logging
from collections.abc import Callable
from typing import Any

from .api import LumaApiError, LumaClient, LumaError
from .const import CONF_CRITERION_CLEARED, CONF_CRITERION_EMAIL, CONF_CRITERION_PASSWORD

_LOGGER = logging.getLogger(__name__)


def account(options: dict[str, Any]) -> tuple[str, str] | None:
    """Email and password, or None unless both are set."""
    email = (options.get(CONF_CRITERION_EMAIL) or "").strip()
    password = options.get(CONF_CRITERION_PASSWORD) or ""
    return (email, password) if email and password else None


def fingerprint(entry_id: str, email: str, password: str) -> str:
    """A hash of the account keyed with the entry ID: equal accounts match, nothing more."""
    message = f"{email}\n{password}".encode()
    return hmac.new(entry_id.encode(), message, hashlib.sha256).hexdigest()


def check(email: str, password: str) -> dict[str, str]:
    """Form errors: both fields or neither, and an email address that looks like one."""
    if bool(email) != bool(password):
        return {"base": "criterion_pair"}
    if email and ("@" not in email or any(c.isspace() for c in email)):
        return {CONF_CRITERION_EMAIL: "criterion_email"}
    return {}


def saved(entry_id: str, current: dict[str, Any], email: str, password: str) -> dict[str, Any]:
    """The options after the account form: cleared fields remember what was cleared."""
    options = {**current, CONF_CRITERION_EMAIL: email, CONF_CRITERION_PASSWORD: password}
    options.pop(CONF_CRITERION_CLEARED, None)
    held = account(current)
    if not (email or password) and held is not None:
        options[CONF_CRITERION_CLEARED] = fingerprint(entry_id, *held)
    elif not (email or password) and current.get(CONF_CRITERION_CLEARED):
        options[CONF_CRITERION_CLEARED] = current[CONF_CRITERION_CLEARED]
    return options


def needs_push(entry_id: str, options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether the TV holds another account than the one held here. A Luma without the
    Criterion player reports nothing and is left alone."""
    if "criterion_account" not in status:
        return False
    held = account(options)
    return held is not None and status["criterion_account"] != fingerprint(entry_id, *held)


def needs_forget(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether the TV still holds the account cleared here."""
    cleared = options.get(CONF_CRITERION_CLEARED)
    return (
        account(options) is None
        and isinstance(cleared, str)
        and status.get("criterion_account") == cleared
    )


async def push(
    client: LumaClient,
    entry_id: str,
    options: dict[str, Any],
    missing_endpoint: Callable[[], None],
    *,
    forget: bool = False,
) -> None:
    """Send the account held here, or with `forget` remove the TV's. Best effort: the TV may be
    off, and the next status check tries again. `missing_endpoint` is called when the TV's Luma
    predates this."""
    held = account(options)
    if held is None and not forget:
        return
    try:
        if held is None:
            await client.forget_criterion_account()
        else:
            await client.criterion_account(*held, fingerprint(entry_id, *held))
    except LumaApiError as err:
        if err.status == 404:
            missing_endpoint()
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
    except LumaError as err:
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
