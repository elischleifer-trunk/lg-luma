"""The Criterion Channel account the TV's Criterion player signs in with.

The owner enters it in this integration's options. Home Assistant sends it to every paired TV,
and again whenever a TV reports it lost it (a reset), so nobody types a password with the
remote. Clearing both fields here removes it from the TVs at their next status check. The TV
hands it only to its own Criterion player, on the Channel's sign-in page.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any

from .api import LumaApiError, LumaClient, LumaError
from .const import CONF_CRITERION_EMAIL, CONF_CRITERION_PASSWORD

_LOGGER = logging.getLogger(__name__)


def account(options: dict[str, Any]) -> tuple[str, str] | None:
    """Email and password, or None unless both are set."""
    email = (options.get(CONF_CRITERION_EMAIL) or "").strip()
    password = options.get(CONF_CRITERION_PASSWORD) or ""
    return (email, password) if email and password else None


def check(email: str, password: str) -> dict[str, str]:
    """Form errors: both fields or neither, and an email address that looks like one."""
    if bool(email) != bool(password):
        return {"base": "criterion_pair"}
    if email and ("@" not in email or any(c.isspace() for c in email)):
        return {CONF_CRITERION_EMAIL: "criterion_email"}
    return {}


def needs_push(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether a TV that reports the account lacks one Home Assistant holds. A Luma without the
    Criterion player reports nothing and is left alone."""
    held = status.get("criterion_account")
    return isinstance(held, bool) and not held and account(options) is not None


def needs_forget(options: dict[str, Any], status: dict[str, Any]) -> bool:
    """Whether a TV holds an account Home Assistant no longer does: it was cleared here, and
    Home Assistant is the only place the TV gets one from."""
    return status.get("criterion_account") is True and account(options) is None


async def push(
    client: LumaClient,
    options: dict[str, Any],
    missing_endpoint: Callable[[], None],
    *,
    forget: bool = False,
) -> None:
    """Send the account held here, or with `forget` remove the TV's when none is held. Best
    effort: the TV may be off. `missing_endpoint` is called when its Luma predates this."""
    held = account(options)
    if held is None and not forget:
        return
    try:
        if held is None:
            await client.forget_criterion_account()
        else:
            await client.criterion_account(*held)
    except LumaApiError as err:
        if err.status == 404:
            missing_endpoint()
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
    except LumaError as err:
        _LOGGER.debug("Could not send the Criterion account to %s: %s", client.host, err)
