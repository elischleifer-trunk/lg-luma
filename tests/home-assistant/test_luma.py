"""Config flow, services and reauth for the Luma integration, with the TV mocked over HTTP."""

from __future__ import annotations

import pytest
import voluptuous as vol
from homeassistant import config_entries
from homeassistant.config_entries import ConfigEntryState
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.exceptions import HomeAssistantError, ServiceValidationError
from pytest_homeassistant_custom_component.common import MockConfigEntry
from pytest_homeassistant_custom_component.test_util.aiohttp import AiohttpClientMocker

try:
    from homeassistant.helpers.service_info.ssdp import SsdpServiceInfo
except ImportError:  # pragma: no cover
    from homeassistant.components.ssdp import SsdpServiceInfo

from custom_components.luma.const import DOMAIN

HOST = "192.168.1.50"
TV = f"http://{HOST}:5862"
DEVICE = "0123456789abcdef0123456789abcdef"
TOKEN = "f" * 64
INFO = {"id": DEVICE, "name": "Luma", "version": "2.18.0", "api": 1}


def tv_pairing(aioclient_mock: AiohttpClientMocker, confirm_status: int = 200) -> None:
    aioclient_mock.get(f"{TV}/api/info", json=INFO)
    aioclient_mock.post(f"{TV}/api/pair", json={"pairing_id": "p1", "expires_in": 120})
    if confirm_status == 200:
        aioclient_mock.post(f"{TV}/api/pair/confirm", json={"token": TOKEN, "id": DEVICE})
    else:
        aioclient_mock.post(
            f"{TV}/api/pair/confirm", status=confirm_status, json={"error": "Wrong code"}
        )


def entry(hass: HomeAssistant) -> MockConfigEntry:
    config = MockConfigEntry(
        domain=DOMAIN,
        unique_id=DEVICE,
        title="Living room TV",
        data={"host": HOST, "port": 5862, "token": TOKEN, "device_id": DEVICE},
    )
    config.add_to_hass(hass)
    return config


async def test_user_flow_pairs_with_the_code_shown_on_the_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    # A TV already set up with the LG webOS integration lends its address and name.
    MockConfigEntry(domain="webostv", title="Living room TV", data={"host": HOST}).add_to_hass(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": config_entries.SOURCE_USER}
    )
    assert result["type"] is FlowResultType.FORM
    schema = result["data_schema"].schema
    assert next(key for key in schema if key == "host").default() == HOST

    tv_pairing(aioclient_mock, confirm_status=403)
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], {"host": HOST, "port": 5862}
    )
    assert result["step_id"] == "pair"
    pair_request = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/pair")
    assert pair_request[2] == {"name": hass.config.location_name}

    result = await hass.config_entries.flow.async_configure(result["flow_id"], {"code": "000000"})
    assert result["errors"] == {"base": "invalid_code"}

    aioclient_mock.clear_requests()
    tv_pairing(aioclient_mock)
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {"code": "123456"})
    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["title"] == "Living room TV"
    assert result["data"] == {"host": HOST, "port": 5862, "token": TOKEN, "device_id": DEVICE}
    assert result["result"].unique_id == DEVICE


async def test_user_flow_reports_a_tv_without_luma(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    aioclient_mock.get(f"{TV}/api/info", json={"name": "Something else"})
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": config_entries.SOURCE_USER}
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], {"host": HOST, "port": 5862}
    )
    assert result["errors"] == {"base": "cannot_connect"}


def ssdp(host: str = HOST) -> SsdpServiceInfo:
    return SsdpServiceInfo(
        ssdp_usn="uuid:tv",
        ssdp_st="urn:lge-com:service:webos-second-screen:1",
        ssdp_location=f"http://{host}:1824/",
        upnp={"friendlyName": "[LG] webOS TV"},
    )


async def test_ssdp_discovery_offers_only_tvs_running_luma(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    aioclient_mock.get(f"{TV}/api/info", status=404)
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": config_entries.SOURCE_SSDP}, data=ssdp()
    )
    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "not_luma"

    aioclient_mock.clear_requests()
    tv_pairing(aioclient_mock)
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": config_entries.SOURCE_SSDP}, data=ssdp()
    )
    assert result["step_id"] == "discovery_confirm"
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {})
    assert result["step_id"] == "pair"
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {"code": "123456"})
    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["title"] == "Luma ([LG] webOS TV)"


async def test_rediscovery_updates_the_address_of_a_known_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = entry(hass)
    aioclient_mock.get("http://192.168.1.77:5862/api/info", json=INFO)
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": config_entries.SOURCE_SSDP}, data=ssdp("192.168.1.77")
    )
    assert result["reason"] == "already_configured"
    assert config.data["host"] == "192.168.1.77"


async def setup(hass: HomeAssistant, aioclient_mock: AiohttpClientMocker) -> MockConfigEntry:
    config = entry(hass)
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "overlay_ready": True})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()
    assert config.state is ConfigEntryState.LOADED
    return config


async def test_show_camera_sends_signed_proxy_urls_and_options(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    await setup(hass, aioclient_mock)
    hass.config.internal_url = "http://192.168.1.10:8123"
    hass.states.async_set(
        "camera.front_door", "idle", {"access_token": "abc", "friendly_name": "Front door"}
    )
    aioclient_mock.post(f"{TV}/api/overlay", json={"session": "s", "paused": True})
    await hass.services.async_call(
        DOMAIN,
        "show_camera",
        {"camera": "camera.front_door", "pause": True, "position": "bottom-left", "duration": 45},
        blocking=True,
    )
    call = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/overlay")
    assert call[3]["Authorization"] == f"Bearer {TOKEN}"
    assert call[2] == {
        "stream_url": "http://192.168.1.10:8123/api/camera_proxy_stream/camera.front_door?token=abc",
        "snapshot_url": "http://192.168.1.10:8123/api/camera_proxy/camera.front_door?token=abc",
        "title": "Front door",
        "duration": 45.0,
        "refresh": 1.0,
        "position": "bottom-left",
        "size": "medium",
        "pause": True,
    }

    # Snapshot mode with a go2rtc URL and an explicit title.
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/overlay", json={"session": "s", "paused": False})
    await hass.services.async_call(
        DOMAIN,
        "show_camera",
        {
            "snapshot_url": "http://nvr:1984/api/frame.jpeg?src=door",
            "mode": "snapshot",
            "title": "Door",
            "refresh": 2,
        },
        blocking=True,
    )
    payload = aioclient_mock.mock_calls[-1][2]
    assert "stream_url" not in payload
    assert payload["snapshot_url"] == "http://nvr:1984/api/frame.jpeg?src=door"
    assert payload["refresh"] == 2.0


async def test_show_camera_rejects_non_cameras_and_missing_sources(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    await setup(hass, aioclient_mock)
    hass.states.async_set("light.porch", "on")
    with pytest.raises(ServiceValidationError):
        await hass.services.async_call(
            DOMAIN, "show_camera", {"camera": "light.porch"}, blocking=True
        )
    with pytest.raises((vol.Invalid, ServiceValidationError)):
        await hass.services.async_call(DOMAIN, "show_camera", {"title": "x"}, blocking=True)


async def test_a_tv_that_forgot_the_server_starts_reauth(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = await setup(hass, aioclient_mock)
    aioclient_mock.post(f"{TV}/api/notify", status=401, json={"error": "Pair again"})
    with pytest.raises(HomeAssistantError):
        await hass.services.async_call(DOMAIN, "notify", {"message": "Hi"}, blocking=True)
    await hass.async_block_till_done()
    flows = hass.config_entries.flow.async_progress_by_handler(DOMAIN)
    assert [flow["context"]["source"] for flow in flows] == ["reauth"]

    # Pairing again replaces the token and reloads the entry.
    new_token = "e" * 64
    aioclient_mock.clear_requests()
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "overlay_ready": True})
    aioclient_mock.post(f"{TV}/api/pair", json={"pairing_id": "p2", "expires_in": 120})
    aioclient_mock.post(f"{TV}/api/pair/confirm", json={"token": new_token, "id": DEVICE})
    result = await hass.config_entries.flow.async_configure(flows[0]["flow_id"], {})
    assert result["step_id"] == "pair"
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {"code": "654321"})
    assert result["reason"] == "reauth_successful"
    assert config.data["token"] == new_token


async def test_setup_with_a_revoked_token_asks_to_pair_again(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = entry(hass)
    aioclient_mock.get(f"{TV}/api/status", status=401, json={"error": "Pair again"})
    await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()
    assert config.state is ConfigEntryState.SETUP_ERROR
    assert hass.config_entries.flow.async_progress_by_handler(DOMAIN)


async def test_an_unreachable_tv_still_loads_and_entities_exist(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = entry(hass)
    aioclient_mock.get(f"{TV}/api/status", exc=TimeoutError)
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()
    assert config.state is ConfigEntryState.LOADED
    assert hass.states.get("button.living_room_tv_dismiss_overlay")
    assert hass.states.get("notify.living_room_tv_toast")


async def test_dismiss_button_and_notify_entity_reach_the_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    await setup(hass, aioclient_mock)
    aioclient_mock.delete(f"{TV}/api/overlay", json={"overlay": None})
    aioclient_mock.post(f"{TV}/api/notify", json={"shown": True})
    await hass.services.async_call(
        "button", "press", {"entity_id": "button.living_room_tv_dismiss_overlay"}, blocking=True
    )
    await hass.services.async_call(
        "notify",
        "send_message",
        {"entity_id": "notify.living_room_tv_toast", "title": "Laundry", "message": "Done"},
        blocking=True,
    )
    paths = [(call[0], call[1].path) for call in aioclient_mock.mock_calls]
    assert ("DELETE", "/api/overlay") in paths
    notify = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/notify")
    assert notify[2] == {"message": "Laundry: Done"}


async def test_example_buttons_preview_the_camera_card_and_toast(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    await setup(hass, aioclient_mock)
    hass.config.internal_url = "http://192.168.1.10:8123"
    aioclient_mock.post(f"{TV}/api/overlay", json={"session": "s1", "paused": False})
    aioclient_mock.post(f"{TV}/api/notify", json={"shown": True})
    for button in ("show_example_camera_card", "show_example_toast"):
        await hass.services.async_call(
            "button", "press", {"entity_id": f"button.living_room_tv_{button}"}, blocking=True
        )
    overlay = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/overlay")
    assert overlay[2]["snapshot_url"] == "http://192.168.1.10:8123/api/luma/example.jpg"
    assert overlay[2]["title"] == "Front door"
    assert "stream_url" not in overlay[2]
    assert overlay[2]["pause"] is False
    notify = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/notify")
    assert notify[2] == {"message": "Example from Home Assistant: the washer is done"}
