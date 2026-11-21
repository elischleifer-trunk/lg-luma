"""The Criterion Channel account the TV's Criterion player signs in with.

The owner enters it in this integration's options. Each save gives the account a new random
fingerprint, kept beside it (never derived from the password). Home Assistant sends the account
with it to every paired TV, and each TV reports the fingerprint of the account it holds, so Home
Assistant sends the account again when the TV holds none (a reset) or one this server sent
earlier (a change made while the TV was off). An account another paired server sent is left
alone, so two servers never take turns overwriting it.

Clearing both fields records the fingerprint of the account that was cleared. A TV still holding
that account is told to forget it; an account another Home Assistant server sent is left alone.
The TV hands the account only to its own Criterion player, on the Channel's sign-in page.
"""

from __future__ import annotations

import logging
import secrets
from collections.abc import Callable
from typing import Any

from .api import LumaApiError, LumaClient, LumaError
from .const import (
    CONF_CRITERION_CLEARED,
    CONF_CRITERION_EARLIER,
    CONF_CRITERION_EMAIL,
    CONF_CRITERION_FINGERPRINT,
    CONF_CRITERION_PASSWORD,
)

_LOGGER = logging.getLogger(__name__)
# Fingerprints of accounts this server sent before, recognized as its own on a TV.
EARLIER_KEPT = 5


def account(options: dict[str, Any]) -> tuple[str, str] | None:
    """Email and password, or None unless both are set."""
    email = (options.get(CONF_CRITERION_EMAIL) or "").strip()
    password = options.get(CONF_CRITERION_PASSWORD) or ""
    return (email, password) if email and password else None


def fingerprint(options: dict[str, Any]) -> str | None:
    """The saved account's random fingerprint, when an account is saved."""
    value = options.get(CONF_CRITERION_FINGERPRINT)
    return value if account(options) is not None and isinstance(value, str) and value else None


def check(email: str, password: str) -> dict[str, str]:
    """Form errors: both fields or neither, and an email address that looks like one."""
    if bool(email) != bool(password):
        return {"base": "criterion_pair"}
    if email and ("@" not in email or any(c.isspace() for c in email)):
        return {CONF_CRITERION_EMAIL: "criterion_email"}
    return {}


def saved(current: dict[str, Any], email: str, password: str) -> dict[str, Any]:
    """The options after the account form. A new or changed account gets a new fingerprint;
    cleared fields remember the fingerprint of what was cleared."""
    options = {**current, CONF_CRITERION_EMAIL: email, CONF_CRITERION_PASSWORD: password}
    options.pop(CONF_CRITERION_CLEARED, None)
    options.pop(CONF_CRITERION_FINGERPRINT, None)
    earlier = [mark for mark in current.get(CONF_CRITERION_EARLIER, []) if isinstance(mark, str)]
    if fingerprint(current) and account(current) != (email, password):
        earlier.append(fingerprint(current))
    options[CONF_CRITERION_EARLIER] = earlier[-EARLIER_KEPT:]
    if email and password:
        unchanged = account(current) == (email, password)
        options[CONF_CRITERION_FINGERPRINT] = (
            fingerprint(current) if unchanged and fingerprint(current) else secrets.token_hex(16)
        )
    elif fingerprint(current):
        options[CONF_CRITERION_CLEARED] = fingerprint(current)
    elif current.get(CONF_CRITERION_CLEARED):
        options[CONF_CRITERION_CLEARED] = current[CONF_CRITERION_CLEARED]
    return options


def needs_push(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether the TV holds no account, or an earlier one from this server. A Luma without the
    Criterion player reports nothing and is left alone, as is another server's account."""
    if "criterion_account" not in status or account(options) is None:
        return False
    held = status["criterion_account"]
    if held is False or held is True:
        return True
    return held in options.get(CONF_CRITERION_EARLIER, [])


def needs_forget(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether the account was cleared here and the TV still holds one this server sent: the
    cleared one, or an earlier one if the TV was off through a change and the clearing."""
    if account(options) is not None:
        return False
    held = status.get("criterion_account")
    sent = [options.get(CONF_CRITERION_CLEARED), *options.get(CONF_CRITERION_EARLIER, [])]
    return isinstance(held, str) and held in [mark for mark in sent if isinstance(mark, str)]


async def push(
    client: LumaClient,
    options: dict[str, Any],
    missing_endpoint: Callable[[], None],
    *,
    forget: bool = False,
) -> bool:
    """Send the account held here, or with `forget` remove the TV's; whether the TV now holds
    the account held here. Best effort: the TV may be off, and the next status check tries
    again. `missing_endpoint` is called when the TV's Luma predates this."""
    held = account(options)
    if held is None and not forget:
        return False
    try:
        if held is None:
            await client.forget_criterion_account()
            return False
        await client.criterion_account(*held, fingerprint(options) or "")
    except LumaApiError as err:
        if err.status == 404:
            missing_endpoint()
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
    except LumaError as err:
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
    else:
        return True
    return False
