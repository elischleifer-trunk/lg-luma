"""The backup of the TV's Criterion Channel sign-in, with the TV mocked over HTTP."""

from __future__ import annotations

import json
import time
from datetime import datetime, timedelta
from typing import Any

import pytest
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers import entity_registry as er
from homeassistant.util import dt as dt_util
from pytest_homeassistant_custom_component.common import MockConfigEntry, async_fire_time_changed
from pytest_homeassistant_custom_component.test_util.aiohttp import AiohttpClientMocker

from custom_components.luma.criterion_session import expiry
from custom_components.luma.diagnostics import async_get_config_entry_diagnostics

HOST = "192.168.1.50"
TV = f"http://{HOST}:5862"
DEVICE = "0123456789abcdef0123456789abcdef"
DAY = 86400
NOW = time.time()
SESSION_URL = f"{TV}/api/criterion-session"


def entry(hass: HomeAssistant) -> MockConfigEntry:
    config = MockConfigEntry(
        domain="luma",
        unique_id=DEVICE,
        title="Living room TV",
        data={"host": HOST, "port": 5862, "token": "f" * 64, "device_id": DEVICE},
    )
    config.add_to_hass(hass)
    return config


def cookies(expires: float) -> list[dict[str, Any]]:
    return [
        {
            "name": "__session",
            "value": f"SECRET-SESSION-{int(expires)}",
            "domain": "www.criterionchannel.com",
            "path": "/",
            "expires": expires,
            "secure": True,
            "httpOnly": True,
            "sameSite": "Lax",
        },
        {
            "name": "login",
            "value": "SECRET-LOGIN",
            "domain": "login.criterion.com",
            "path": "/",
            "expires": None,
            "secure": True,
            "httpOnly": True,
            "sameSite": None,
        },
    ]


def storage_key(config: MockConfigEntry) -> str:
    return f"luma.criterion_session.{config.entry_id}"


def back_up(hass_storage: dict[str, Any], config: MockConfigEntry, expires: float) -> None:
    hass_storage[storage_key(config)] = {
        "version": 1,
        "key": storage_key(config),
        "data": {"cookies": cookies(expires), "expires": expires},
    }


def session_calls(aioclient_mock: AiohttpClientMocker) -> list[tuple[str, Any]]:
    return [
        (call[0], call[2])
        for call in aioclient_mock.mock_calls
        if call[1].path == "/api/criterion-session"
    ]


def tv(
    aioclient_mock: AiohttpClientMocker,
    reported: Any,
    *,
    held: float | None = None,
    stored: bool = True,
    account: Any = False,
) -> None:
    """The TV reporting `reported` ("absent": an older Luma), holding a session that expires at
    `held`, answering an offered one with `stored`, and holding the Criterion account with
    fingerprint `account` (False: none)."""
    aioclient_mock.clear_requests()
    status: dict[str, Any] = {"overlay": None, "overlay_ready": True, "cameras": 0}
    if reported != "absent":
        status["criterion_session"] = reported
        status["criterion_account"] = account
    aioclient_mock.get(f"{TV}/api/status", json=status)
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    aioclient_mock.put(f"{TV}/api/criterion-account", json={"criterion_account": True})
    if held is None:
        aioclient_mock.get(SESSION_URL, status=404, json={"error": "No session"})
    else:
        aioclient_mock.get(SESSION_URL, json={"cookies": cookies(held), "expires": held})
    aioclient_mock.put(
        SESSION_URL,
        json={
            "criterion_session": reported if isinstance(reported, dict) else None,
            "stored": stored,
        },
    )
    aioclient_mock.delete(SESSION_URL, json={"criterion_session": None})


def reports(expires: float, valid: bool = True) -> dict[str, Any]:
    return {"expires": expires, "checked": NOW - 60, "valid": valid}


async def start(hass: HomeAssistant, config: MockConfigEntry) -> None:
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)


async def check(hass: HomeAssistant) -> None:
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)


@pytest.fixture(autouse=True)
def no_camera_delay(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)


def test_only_a_cookie_list_the_tv_would_take_counts() -> None:
    expires = NOW + 30 * DAY
    assert expiry(cookies(expires)) == expires
    dotted = [{**cookies(expires)[0], "domain": ".criterionchannel.com"}]
    assert expiry(dotted) == expires
    # Counted the way the TV counts it: any spelling of the Channel's hosts, never an empty one.
    for domain in (".www.criterionchannel.com", "criterionchannel.com"):
        assert expiry([{**cookies(expires)[0], "domain": domain}]) == expires
    assert expiry([{**cookies(expires)[0], "value": ""}]) is None
    assert expiry([]) is None
    assert expiry("cookies") is None
    assert expiry(cookies(expires) * 33) is None
    # No __session cookie, or one without an expiry, or on the sign-in domain only.
    assert expiry(cookies(expires)[1:]) is None
    assert expiry([{**cookies(expires)[0], "expires": None}]) is None
    assert expiry([{**cookies(expires)[0], "expires": True}]) is None
    assert expiry([{**cookies(expires)[0], "domain": "login.criterion.com"}]) is None
    # Other sites, and lookalikes, are refused.
    for domain in ("example.com", "notcriterion.com", "criterion.com.evil.net"):
        assert expiry([*cookies(expires), {**cookies(expires)[1], "domain": domain}]) is None
    assert expiry([*cookies(expires), {**cookies(expires)[1], "name": "x" * 257}]) is None
    assert expiry([*cookies(expires), {**cookies(expires)[1], "name": ""}]) is None
    assert expiry([*cookies(expires), {**cookies(expires)[1], "value": "x" * 8193}]) is None
    assert expiry([*cookies(expires), {**cookies(expires)[1], "value": 5}]) is None


@pytest.mark.parametrize("backup", [None, NOW + 10 * DAY])
async def test_a_newer_sign_in_on_the_tv_is_backed_up(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_storage: dict[str, Any],
    backup: float | None,
) -> None:
    config = entry(hass)
    if backup is not None:
        back_up(hass_storage, config, backup)
    newer = NOW + 30 * DAY
    tv(aioclient_mock, reports(newer), held=newer)
    await start(hass, config)
    assert session_calls(aioclient_mock) == [("GET", None)]
    assert hass_storage[storage_key(config)]["data"] == {
        "cookies": cookies(newer),
        "expires": newer,
    }

    # Backed up: the next check fetches nothing.
    tv(aioclient_mock, reports(newer), held=newer)
    await check(hass)
    assert session_calls(aioclient_mock) == []

    registry = er.async_get(hass)
    entity_id = registry.async_get_entity_id(
        "sensor", "luma", f"{DEVICE}_criterion_session_expires"
    )
    assert entity_id is not None
    state = hass.states.get(entity_id)
    assert datetime.fromisoformat(state.state).timestamp() == pytest.approx(newer)
    assert "SECRET" not in json.dumps(dict(state.attributes))


async def test_a_signed_out_sign_in_on_the_tv_is_not_backed_up(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    newer = NOW + 30 * DAY
    tv(aioclient_mock, reports(newer, valid=False), held=newer)
    await start(hass, config)
    assert session_calls(aioclient_mock) == []
    assert storage_key(config) not in hass_storage


@pytest.mark.parametrize(
    "reported",
    [
        None,  # A reset.
        reports(NOW + 5 * DAY),  # An older sign-in.
        reports(NOW + 30 * DAY, valid=False),  # One the TV found signed out.
    ],
)
async def test_the_tv_gets_the_backup_when_it_has_none_or_an_older_one(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_storage: dict[str, Any],
    reported: dict[str, Any] | None,
) -> None:
    config = entry(hass)
    backup = NOW + 20 * DAY
    back_up(hass_storage, config, backup)
    tv(aioclient_mock, reported)
    await start(hass, config)
    assert session_calls(aioclient_mock) == [("PUT", {"cookies": cookies(backup)})]


async def test_a_refused_backup_is_not_offered_again(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    backup = NOW + 20 * DAY
    back_up(hass_storage, config, backup)
    tv(aioclient_mock, reports(NOW + 5 * DAY), stored=False)
    await start(hass, config)
    assert [method for method, _ in session_calls(aioclient_mock)] == ["PUT"]
    tv(aioclient_mock, reports(NOW + 5 * DAY), stored=False)
    await check(hass)
    await check(hass)
    assert session_calls(aioclient_mock) == []


async def test_an_expired_backup_is_not_offered(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    back_up(hass_storage, config, NOW - DAY)
    tv(aioclient_mock, None)
    await start(hass, config)
    assert session_calls(aioclient_mock) == []


async def test_an_older_luma_is_left_alone(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    back_up(hass_storage, config, NOW + 20 * DAY)
    tv(aioclient_mock, "absent")
    await start(hass, config)
    await check(hass)
    assert session_calls(aioclient_mock) == []
    entity_id = er.async_get(hass).async_get_entity_id(
        "sensor", "luma", f"{DEVICE}_criterion_session_expires"
    )
    assert hass.states.get(entity_id).state == "unknown"


async def test_the_owner_is_told_once_when_the_tv_is_signed_out(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = entry(hass)
    ended = NOW + 25 * DAY
    tv(aioclient_mock, reports(ended, valid=False))
    await start(hass, config)
    notice_id = f"luma_criterion_session_{config.entry_id}"
    notices = hass.data["persistent_notification"]
    assert notice_id in notices
    assert "sign in again" in notices[notice_id]["message"]
    # Dismissed by the owner: not raised again for the same sign-in.
    notices.pop(notice_id)
    tv(aioclient_mock, reports(ended, valid=False))
    await check(hass)
    assert notice_id not in notices
    # Signed in again: nothing to say.
    tv(aioclient_mock, reports(ended + DAY), held=ended + DAY)
    await check(hass)
    assert notice_id not in notices


async def test_clearing_the_account_forgets_the_sign_in(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config,
        options={
            "criterion_email": "me@example.com",
            "criterion_password": "pw",
            "criterion_fingerprint": "f00d",
        },
    )
    expires = NOW + 20 * DAY
    back_up(hass_storage, config, expires)
    tv(aioclient_mock, reports(expires), held=expires, account="f00d")
    await start(hass, config)
    assert session_calls(aioclient_mock) == []

    tv(aioclient_mock, reports(expires), held=expires, account="f00d")
    aioclient_mock.delete(f"{TV}/api/criterion-account", json={"criterion_account": False})
    result = await hass.config_entries.options.async_init(config.entry_id)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"next_step_id": "criterion_account"}
    )
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"criterion_email": "", "criterion_password": ""}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    assert session_calls(aioclient_mock) == [("DELETE", None)]
    assert storage_key(config) not in hass_storage

    # Not backed up again from a TV that still reports it.
    tv(aioclient_mock, None)
    await check(hass)
    assert session_calls(aioclient_mock) == []
    assert storage_key(config) not in hass_storage


async def test_a_tv_off_while_cleared_forgets_the_sign_in_when_it_answers(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config,
        options={
            "criterion_email": "me@example.com",
            "criterion_password": "pw",
            "criterion_fingerprint": "f00d",
        },
    )
    expires = NOW + 20 * DAY
    back_up(hass_storage, config, expires)
    aioclient_mock.get(f"{TV}/api/status", exc=TimeoutError)
    aioclient_mock.put(f"{TV}/api/criterion-account", exc=TimeoutError)
    aioclient_mock.post(f"{TV}/api/cameras", exc=TimeoutError)
    await start(hass, config)
    # As the options form saves a clearing: with the cleared account's fingerprint.
    hass.config_entries.async_update_entry(
        config,
        options={"criterion_email": "", "criterion_password": "", "criterion_cleared": "f00d"},
    )
    await hass.async_block_till_done()
    # The cookies are gone at once; the TV is told when it answers.
    assert "cookies" not in hass_storage[storage_key(config)]["data"]
    tv(aioclient_mock, reports(expires), held=expires, account="f00d")
    aioclient_mock.delete(f"{TV}/api/criterion-account", json={"criterion_account": False})
    await check(hass)
    assert session_calls(aioclient_mock) == [("DELETE", None)]
    assert storage_key(config) not in hass_storage


ACCOUNT = {
    "criterion_email": "me@example.com",
    "criterion_password": "pw",
    "criterion_fingerprint": "f00d",
}


async def test_a_sign_in_with_this_servers_account_is_looked_after(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    hass.config_entries.async_update_entry(config, options=ACCOUNT)
    newer = NOW + 30 * DAY
    tv(aioclient_mock, reports(newer), held=newer, account="f00d")
    await start(hass, config)
    assert session_calls(aioclient_mock) == [("GET", None)]
    assert hass_storage[storage_key(config)]["data"]["expires"] == newer


async def test_a_backup_waits_until_the_tv_holds_this_servers_account(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    # Not yet sent to the TV: the sign-in there is not this account's.
    config = entry(hass)
    hass.config_entries.async_update_entry(config, options=ACCOUNT)
    back_up(hass_storage, config, NOW + 20 * DAY)
    tv(aioclient_mock, None, account=False)
    await start(hass, config)
    assert session_calls(aioclient_mock) == []


@pytest.mark.parametrize("options", [{}, ACCOUNT])
async def test_another_servers_sign_in_is_left_alone(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_storage: dict[str, Any],
    options: dict[str, Any],
) -> None:
    config = entry(hass)
    hass.config_entries.async_update_entry(config, options=options)
    back_up(hass_storage, config, NOW + 20 * DAY)
    # The TV holds another server's account, and a sign-in this server's copy would replace.
    tv(aioclient_mock, reports(NOW + 30 * DAY, valid=False), held=NOW + 30 * DAY, account="beef")
    await start(hass, config)
    await check(hass)
    assert session_calls(aioclient_mock) == []
    # Its copy is dropped, so it is not restored once the other account is cleared.
    assert storage_key(config) not in hass_storage
    notices = hass.data.get("persistent_notification", {})
    assert f"luma_criterion_session_{config.entry_id}" not in notices
    tv(aioclient_mock, None, account=False if not options else "beef")
    await check(hass)
    assert session_calls(aioclient_mock) == []


async def test_clearing_the_account_leaves_another_servers_sign_in(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    hass.config_entries.async_update_entry(config, options=ACCOUNT)
    expires = NOW + 20 * DAY
    tv(aioclient_mock, reports(expires), held=expires, account="beef")
    await start(hass, config)
    hass.config_entries.async_update_entry(
        config,
        options={"criterion_email": "", "criterion_password": "", "criterion_cleared": "f00d"},
    )
    await hass.async_block_till_done()
    tv(aioclient_mock, reports(expires), held=expires, account="beef")
    await check(hass)
    assert session_calls(aioclient_mock) == []
    assert storage_key(config) not in hass_storage


async def test_diagnostics_never_show_the_sign_in(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, hass_storage: dict[str, Any]
) -> None:
    config = entry(hass)
    expires = NOW + 30 * DAY
    tv(aioclient_mock, reports(expires), held=expires)
    await start(hass, config)
    assert storage_key(config) in hass_storage
    aioclient_mock.get(f"{TV}/api/diagnostics", json={"boot": "ab", "entries": []})
    diagnostics = await async_get_config_entry_diagnostics(hass, config)
    assert "SECRET" not in json.dumps(diagnostics)
    assert diagnostics["criterion_session"]["backup_expires"] == expires
    assert diagnostics["criterion_session"]["tv"]["valid"] is True
