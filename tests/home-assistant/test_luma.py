"""Config flow, services and reauth for the Luma integration, with the TV mocked over HTTP."""

from __future__ import annotations

import asyncio
from datetime import timedelta

import pytest
import voluptuous as vol
from homeassistant import config_entries
from homeassistant.config_entries import ConfigEntryState
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.exceptions import HomeAssistantError, ServiceValidationError
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import instance_id
from homeassistant.helpers import issue_registry as ir
from homeassistant.util import dt as dt_util
from pytest_homeassistant_custom_component.common import (
    MockConfigEntry,
    async_fire_time_changed,
    async_mock_service,
)
from pytest_homeassistant_custom_component.test_util.aiohttp import AiohttpClientMocker

try:
    from homeassistant.helpers.service_info.ssdp import SsdpServiceInfo
except ImportError:  # pragma: no cover
    from homeassistant.components.ssdp import SsdpServiceInfo

from custom_components.luma import events as luma_events
from custom_components.luma.api import LumaAuthError, LumaClient
from custom_components.luma.const import DOMAIN, INTEGRATION_API
from custom_components.luma.diagnostics import async_get_config_entry_diagnostics

# The autouse `silent_tv_events` fixture turns the stream off; these tests turn it back on.
REAL_EVENTS = LumaClient.events
REAL_FOLLOW = luma_events.LumaEvents._follow

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
    # The instance ID lets Luma give this server's camera arrangement back if it pairs again.
    assert pair_request[2] == {
        "name": hass.config.location_name,
        "server": await instance_id.async_get(hass),
    }

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


async def test_video_mode_sends_the_cameras_hls_playlist_with_mjpeg_fallbacks(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    await setup(hass, aioclient_mock)
    hass.config.internal_url = "http://192.168.1.10:8123"
    hass.config.components.add("stream")
    streams = {"supported_features": 2}
    door = {"access_token": "d1", "friendly_name": "Door", **streams}
    hass.states.async_set("camera.door", "idle", door)
    hass.states.async_set(
        "camera.still", "idle", {"access_token": "s1", "friendly_name": "Still", **streams}
    )
    hass.states.async_set("camera.yard", "idle", {"access_token": "y1", "friendly_name": "Yard"})
    requested = []

    # How camera.still fails to start a stream; None makes it too slow instead.
    still: dict[str, Exception | None] = {
        "failure": HomeAssistantError("camera.still does not support play stream service")
    }

    async def request_stream(hass_: HomeAssistant, entity_id: str) -> str:
        requested.append(entity_id)
        if entity_id == "camera.still":
            if still["failure"] is None:
                await asyncio.sleep(1)
            else:
                raise still["failure"]
        return "/api/hls/abc/master_playlist.m3u8"

    monkeypatch.setattr("custom_components.luma._request_stream", request_stream)
    monkeypatch.setattr("custom_components.luma.HLS_START_TIMEOUT", 0.05)
    aioclient_mock.post(f"{TV}/api/overlay", json={"session": "s", "paused": False})

    await hass.services.async_call(
        DOMAIN, "show_camera", {"camera": "camera.door", "mode": "video"}, blocking=True
    )
    payload = aioclient_mock.mock_calls[-1][2]
    assert payload["hls_url"] == "http://192.168.1.10:8123/api/hls/abc/master_playlist.m3u8"
    assert payload["stream_url"] == (
        "http://192.168.1.10:8123/api/camera_proxy_stream/camera.door?token=d1"
    )
    assert (
        payload["snapshot_url"] == "http://192.168.1.10:8123/api/camera_proxy/camera.door?token=d1"
    )

    # A camera that cannot stream shows its motion-JPEG, as in stream mode.
    await hass.services.async_call(
        DOMAIN, "show_camera", {"camera": "camera.still", "mode": "video"}, blocking=True
    )
    payload = aioclient_mock.mock_calls[-1][2]
    assert "hls_url" not in payload
    assert payload["stream_url"].startswith("http://192.168.1.10:8123/api/camera_proxy_stream/")

    # Any other failure, or a stream that takes too long to start, never holds back the card.
    for failure in (OSError("camera cloud unreachable"), None):
        still["failure"] = failure
        await hass.services.async_call(
            DOMAIN, "show_camera", {"camera": "camera.still", "mode": "video"}, blocking=True
        )
        payload = aioclient_mock.mock_calls[-1][2]
        assert "hls_url" not in payload
        assert payload["stream_url"].startswith("http://192.168.1.10:8123/")

    # Video is the default; stream mode never starts an HLS stream.
    await hass.services.async_call(DOMAIN, "show_camera", {"camera": "camera.door"}, blocking=True)
    assert "hls_url" in aioclient_mock.mock_calls[-1][2]
    await hass.services.async_call(
        DOMAIN, "show_camera", {"camera": "camera.door", "mode": "stream"}, blocking=True
    )
    assert "hls_url" not in aioclient_mock.mock_calls[-1][2]
    # A camera without the stream feature never asks for one.
    await hass.services.async_call(DOMAIN, "show_camera", {"camera": "camera.yard"}, blocking=True)
    assert "hls_url" not in aioclient_mock.mock_calls[-1][2]
    assert requested == [
        "camera.door",
        "camera.still",
        "camera.still",
        "camera.still",
        "camera.door",
    ]


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
    # What else the entry keeps in its data must survive pairing again.
    hass.config_entries.async_update_entry(
        config, data={**config.data, "dev_access_token_hash": "kept"}
    )
    webhook_id = config.data["webhook_id"]

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
    assert config.data["webhook_id"] == webhook_id
    assert config.data["dev_access_token_hash"] == "kept"


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


async def test_every_camera_is_sent_to_the_tv_and_again_when_tokens_rotate(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    hass.states.async_set("camera.yard", "idle", {"access_token": "y1", "friendly_name": "Yard"})
    hass.states.async_set(
        "camera.door", "streaming", {"access_token": "d1", "friendly_name": "Front door"}
    )
    hass.states.async_set("camera.attic", "unavailable", {"friendly_name": "Attic"})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 2})
    await setup(hass, aioclient_mock)
    await hass.async_block_till_done(wait_background_tasks=True)

    def pushes() -> list:
        return [call for call in aioclient_mock.mock_calls if call[1].path == "/api/cameras"]

    assert len(pushes()) == 1
    assert pushes()[0][3]["Authorization"] == f"Bearer {TOKEN}"
    assert pushes()[0][2] == {
        "cameras": [
            {
                "id": "camera.door",
                "name": "Front door",
                "stream_url": "http://192.168.1.10:8123/api/camera_proxy_stream/camera.door?token=d1",
                "snapshot_url": "http://192.168.1.10:8123/api/camera_proxy/camera.door?token=d1",
            },
            {
                "id": "camera.yard",
                "name": "Yard",
                "stream_url": "http://192.168.1.10:8123/api/camera_proxy_stream/camera.yard?token=y1",
                "snapshot_url": "http://192.168.1.10:8123/api/camera_proxy/camera.yard?token=y1",
            },
        ]
    }

    # A token rotation sends the whole list again; other entities do not.
    hass.states.async_set("light.porch", "on")
    await hass.async_block_till_done(wait_background_tasks=True)
    assert len(pushes()) == 1
    hass.states.async_set("camera.yard", "idle", {"access_token": "y2", "friendly_name": "Yard"})
    await hass.async_block_till_done(wait_background_tasks=True)
    assert len(pushes()) == 2
    assert pushes()[1][2]["cameras"][1]["snapshot_url"].endswith("token=y2")


async def test_a_restarted_tv_without_cameras_gets_the_list_again(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    hass.states.async_set("camera.door", "idle", {"access_token": "d1", "friendly_name": "Door"})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 1})
    config = entry(hass)
    aioclient_mock.get(
        f"{TV}/api/status", json={"overlay": None, "overlay_ready": True, "cameras": 0}
    )
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)

    def pushes() -> int:
        return sum(call[1].path == "/api/cameras" for call in aioclient_mock.mock_calls)

    assert pushes() == 1
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert pushes() == 2


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


async def test_streaming_cameras_get_an_hls_url_signed_with_their_token(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    hass.config.components.add("stream")
    door = {"access_token": "d1", "friendly_name": "Door", "supported_features": 2}
    hass.states.async_set("camera.door", "idle", door)
    hass.states.async_set("camera.yard", "idle", {"access_token": "y1", "friendly_name": "Yard"})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 2})
    await setup(hass, aioclient_mock)
    await hass.async_block_till_done(wait_background_tasks=True)
    push = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/cameras")
    door, yard = push[2]["cameras"]
    assert door["hls_url"] == "http://192.168.1.10:8123/api/luma/stream/camera.door?token=d1"
    # A camera without the stream feature keeps only its MJPEG and snapshot URLs.
    assert "hls_url" not in yard


async def test_frigates_copies_of_cameras_start_hidden_in_the_gallery(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    from homeassistant.helpers import entity_registry as er

    from custom_components.luma import camera_list

    hass.config.internal_url = "http://192.168.1.10:8123"
    registry = er.async_get(hass)
    registry.async_get_or_create("camera", "frigate", "f1", suggested_object_id="frontdoor")
    registry.async_get_or_create("camera", "unifiprotect", "u1", suggested_object_id="front_door")
    for entity in ("camera.frontdoor", "camera.front_door", "camera.plain"):
        hass.states.async_set(entity, "idle", {"access_token": "t", "friendly_name": entity})
    # Frigate's copy starts hidden; the recorder's own camera and one outside the registry do not.
    assert [(camera["id"], camera.get("hidden", False)) for camera in camera_list(hass)] == [
        ("camera.front_door", False),
        ("camera.frontdoor", True),
        ("camera.plain", False),
    ]
    # With no other integration's camera to list (one that is unavailable does not count),
    # Frigate's are the cameras: shown.
    hass.states.async_set("camera.front_door", "unavailable", {"access_token": "t"})
    hass.states.async_remove("camera.plain")
    assert [camera.get("hidden", False) for camera in camera_list(hass)] == [False]


async def test_the_stream_view_starts_hls_only_with_the_camera_token(
    hass: HomeAssistant, hass_client_no_auth, monkeypatch: pytest.MonkeyPatch
) -> None:
    from homeassistant.setup import async_setup_component

    assert await async_setup_component(hass, "http", {})
    assert await async_setup_component(hass, DOMAIN, {})

    class Camera:
        def __init__(self) -> None:
            self.access_tokens = ["old", "new"]

    requested = []

    async def request_stream(hass_: HomeAssistant, entity_id: str) -> str:
        requested.append((entity_id, "hls"))
        if entity_id == "camera.still":
            raise HomeAssistantError("camera.still does not support play stream service")
        return "/api/hls/abc/master_playlist.m3u8"

    monkeypatch.setattr(
        "custom_components.luma._camera",
        lambda _hass, entity_id: Camera() if entity_id in ("camera.door", "camera.still") else None,
    )
    monkeypatch.setattr("custom_components.luma._request_stream", request_stream)
    client = await hass_client_no_auth()

    response = await client.get("/api/luma/stream/camera.door?token=new")
    assert response.status == 200
    assert await response.json() == {"url": "/api/hls/abc/master_playlist.m3u8"}
    # The previous token still works, as for Home Assistant's own camera proxy.
    assert (await client.get("/api/luma/stream/camera.door?token=old")).status == 200
    assert requested == [("camera.door", "hls"), ("camera.door", "hls")]

    assert (await client.get("/api/luma/stream/camera.door?token=guess")).status == 403
    assert (await client.get("/api/luma/stream/camera.door")).status == 403
    assert (await client.get("/api/luma/stream/camera.gone?token=new")).status == 404
    assert (await client.get("/api/luma/stream/light.porch?token=new")).status == 404
    assert (await client.get("/api/luma/stream/camera.still?token=new")).status == 404
    assert len(requested) == 3


def protect_camera(hass: HomeAssistant, entity_id: str = "camera.door") -> str:
    """Registers a UniFi Protect camera (and its sensors) as the Protect integration would."""
    from homeassistant.helpers import device_registry as dr
    from homeassistant.helpers import entity_registry as er

    protect = MockConfigEntry(domain="unifiprotect", title="UNVR")
    protect.add_to_hass(hass)
    hass.config.components.add("unifiprotect")
    device = dr.async_get(hass).async_get_or_create(
        config_entry_id=protect.entry_id, identifiers={("unifiprotect", "aa:bb")}
    )
    registry = er.async_get(hass)
    name = entity_id.split(".")[1]
    registry.async_get_or_create(
        "camera",
        "unifiprotect",
        "aabb_high",
        suggested_object_id=name,
        config_entry=protect,
        device_id=device.id,
    )
    # Protect's unique IDs are the device's MAC address and the entity's own key.
    for domain, key, object_id in (
        ("binary_sensor", "motion", f"{name}_motion"),
        ("binary_sensor", "smart_obj_person", f"{name}_person_detected"),
        ("binary_sensor", "dark", f"{name}_is_dark"),
        ("event", "doorbell", f"{name}_doorbell"),
    ):
        registry.async_get_or_create(
            domain,
            "unifiprotect",
            f"aabb_{key}",
            suggested_object_id=object_id,
            config_entry=protect,
            device_id=device.id,
        )
    return protect.entry_id


async def test_unifi_protect_cameras_get_a_history_url_signed_with_their_token(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    protect_camera(hass)
    hass.states.async_set("camera.door", "idle", {"access_token": "d1", "friendly_name": "Door"})
    hass.states.async_set("camera.yard", "idle", {"access_token": "y1", "friendly_name": "Yard"})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 2})
    await setup(hass, aioclient_mock)
    await hass.async_block_till_done(wait_background_tasks=True)
    push = next(call for call in aioclient_mock.mock_calls if call[1].path == "/api/cameras")
    door, yard = push[2]["cameras"]
    assert door["history_url"] == "http://192.168.1.10:8123/api/luma/history/camera.door?token=d1"
    # Only Protect records a camera.
    assert "history_url" not in yard


async def test_the_history_view_redirects_to_protect_with_a_signed_path(
    hass: HomeAssistant, hass_client_no_auth, monkeypatch: pytest.MonkeyPatch
) -> None:
    from aiohttp import web
    from homeassistant.components.http import HomeAssistantView
    from homeassistant.setup import async_setup_component

    assert await async_setup_component(hass, "http", {})
    assert await async_setup_component(hass, DOMAIN, {})
    nvr = protect_camera(hass)

    class Camera:
        access_tokens = ("t",)

    monkeypatch.setattr(
        "custom_components.luma._camera",
        lambda _hass, entity_id: Camera() if entity_id in ("camera.door", "camera.yard") else None,
    )
    seen = []

    class Protect(HomeAssistantView):
        """Stands in for the UniFi Protect integration's views, which need Home Assistant auth."""

        url = "/api/unifiprotect/{kind}/{nvr}/{camera}/{rest:.*}"
        name = "test:protect"
        requires_auth = True

        async def get(self, request: web.Request, **parts: str) -> web.Response:
            seen.append((parts, dict(request.query)))
            return web.Response(body=b"ok")

    hass.http.register_view(Protect())
    client = await hass_client_no_auth()
    now = int(dt_util.utcnow().timestamp())
    moment = dt_util.utc_from_timestamp(now - 3600).isoformat()

    response = await client.get(f"/api/luma/history/camera.door?token=t&at={now - 3600}&width=1280")
    assert response.status == 200
    parts, query = seen[-1]
    assert parts == {"kind": "snapshot", "nvr": nvr, "camera": "camera.door", "rest": moment}
    # The TV picks the size; Home Assistant's signature covers the rest.
    assert query["width"] == "1280" and "authSig" in query

    response = await client.get(
        f"/api/luma/history/camera.door?token=t&start={now - 600}&end={now - 300}"
    )
    assert response.status == 200
    assert seen[-1][0]["kind"] == "video"
    assert seen[-1][0]["rest"].count("+00:00") == 2

    def status(query: str, camera: str = "camera.door"):
        return client.get(f"/api/luma/history/{camera}?{query}", allow_redirects=False)

    redirect = await status(f"token=t&at={now}")
    assert redirect.status == 302
    assert redirect.headers["Cache-Control"] == "no-store"
    assert (await status(f"token=guess&at={now}")).status == 403
    assert (await status(f"token=t&at={now}", "camera.yard")).status == 404
    assert (await status("token=t&at=yesterday")).status == 400
    assert (await status(f"token=t&at={now - 40 * 86400}")).status == 400
    assert (await status(f"token=t&start={now - 3600}&end={now}")).status == 400
    assert (await status(f"token=t&start={now}&end={now - 60}")).status == 400
    # Without the signature, Protect's views turn the TV away.
    assert (await client.get(f"/api/unifiprotect/snapshot/{nvr}/camera.door/x")).status == 401


async def test_history_events_come_from_the_cameras_protect_sensors(
    hass: HomeAssistant, hass_client_no_auth, monkeypatch: pytest.MonkeyPatch
) -> None:
    from homeassistant.core import State
    from homeassistant.setup import async_setup_component

    from custom_components.luma import event_spans, protect_sensors

    assert await async_setup_component(hass, "http", {})
    assert await async_setup_component(hass, DOMAIN, {})
    protect_camera(hass)
    assert protect_sensors(hass, "camera.door") == {
        "binary_sensor.door_motion": "motion",
        "binary_sensor.door_person_detected": "person",
        "event.door_doorbell": "doorbell",
    }
    start = dt_util.utcnow() - timedelta(hours=1)
    end = dt_util.utcnow()

    def at(minutes: int):
        return start + timedelta(minutes=minutes)

    def state(entity_id: str, value: str, minutes: int) -> State:
        return State(entity_id, value, last_changed=at(minutes), last_updated=at(minutes))

    history = {
        # On before the window: the span starts with the window.
        "binary_sensor.door_motion": [
            state("binary_sensor.door_motion", "on", 0),
            state("binary_sensor.door_motion", "off", 2),
            state("binary_sensor.door_motion", "on", 50),
        ],
        "binary_sensor.door_person_detected": [
            state("binary_sensor.door_person_detected", "off", 0),
            state("binary_sensor.door_person_detected", "on", 10),
            state("binary_sensor.door_person_detected", "off", 11),
        ],
        "event.door_doorbell": [
            state("event.door_doorbell", "unknown", 0),
            state("event.door_doorbell", at(30).isoformat(), 30),
        ],
    }
    spans = event_spans(history, protect_sensors(hass, "camera.door"), start, end)
    seconds = lambda minutes: int(at(minutes).timestamp())  # noqa: E731
    assert spans == [
        {"start": seconds(0), "end": seconds(2), "kind": "motion"},
        {"start": seconds(10), "end": seconds(11), "kind": "person"},
        {"start": seconds(30), "end": seconds(30), "kind": "doorbell"},
        # Still on: until the end of the window.
        {"start": seconds(50), "end": int(end.timestamp()), "kind": "motion"},
    ]

    class Camera:
        access_tokens = ("t",)

    asked = []

    async def sensor_history(_hass, entity_ids, start_, end_):
        asked.append(entity_ids)
        return history

    monkeypatch.setattr("custom_components.luma._camera", lambda _hass, _entity_id: Camera())
    monkeypatch.setattr("custom_components.luma._sensor_history", sensor_history)
    client = await hass_client_no_auth()
    first, last = int(start.timestamp()), int(end.timestamp())
    response = await client.get(
        f"/api/luma/history/camera.door?token=t&events=1&start={first}&end={last}"
    )
    assert response.status == 200
    assert len((await response.json())["events"]) == 4
    assert asked == [list(protect_sensors(hass, "camera.door"))]


async def test_a_doorbell_cameras_own_name_does_not_make_every_sensor_a_doorbell(
    hass: HomeAssistant,
) -> None:
    from custom_components.luma import protect_sensors

    protect_camera(hass, "camera.front_doorbell")
    # Sensors are told apart by their own keys, not the device name in their entity IDs.
    assert protect_sensors(hass, "camera.front_doorbell") == {
        "binary_sensor.front_doorbell_motion": "motion",
        "binary_sensor.front_doorbell_person_detected": "person",
        "event.front_doorbell_doorbell": "doorbell",
    }


MOTN = "motn-key-0123456789abcdef"
TMDB_KEY = "eyJhbGciOiJIUzI1NiJ9.e30.signature"
MOTN_CHECK = "https://api.movieofthenight.com/v4/genres"
TMDB_CHECK = "https://api.themoviedb.org/3/find/tt0087332?external_source=imdb_id"
MDBLIST = "abcdefghij0123456789wxyz"
MDBLIST_CHECK = "https://api.mdblist.com/user"


def key_pushes(aioclient_mock: AiohttpClientMocker) -> list:
    return [
        call[2]
        for call in aioclient_mock.mock_calls
        if call[0] == "PUT" and call[1].path == "/api/streaming-keys"
    ]


async def keys_form(hass: HomeAssistant, config: MockConfigEntry):
    """Configure opens a menu; streaming search keys are one of its pages."""
    result = await hass.config_entries.options.async_init(config.entry_id)
    assert result["type"] is FlowResultType.MENU
    assert result["menu_options"] == [
        "phone_keyboard",
        "streaming_keys",
        "criterion_account",
        "developer_access",
    ]
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"next_step_id": "streaming_keys"}
    )
    assert result["step_id"] == "streaming_keys"
    return result


async def test_options_check_streaming_keys_then_send_them_to_the_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = await setup(hass, aioclient_mock)
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True})
    aioclient_mock.get(MOTN_CHECK, status=403, json={"message": "Not subscribed"})
    aioclient_mock.get(TMDB_CHECK, json={"movie_results": []})

    result = await keys_form(hass, config)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"motn_api_key": f" {MOTN} ", "tmdb_read_access_token": TMDB_KEY}
    )
    # A refused key keeps the form open and nothing reaches the TV.
    assert result["type"] is FlowResultType.FORM
    assert result["errors"] == {"motn_api_key": "motn_refused"}
    assert key_pushes(aioclient_mock) == []

    # MDBList checks its key on the account endpoint, with the key as a query parameter.
    aioclient_mock.clear_requests()
    aioclient_mock.get(MOTN_CHECK, json=[{"id": "action", "name": "Action"}])
    aioclient_mock.get(TMDB_CHECK, json={"movie_results": []})
    aioclient_mock.get(MDBLIST_CHECK, status=401, json={"error": "Invalid API key"})
    result = await hass.config_entries.options.async_configure(
        result["flow_id"],
        {"motn_api_key": MOTN, "tmdb_read_access_token": TMDB_KEY, "mdblist_api_key": "wrong"},
    )
    assert result["errors"] == {"mdblist_api_key": "mdblist_refused"}
    assert key_pushes(aioclient_mock) == []

    aioclient_mock.clear_requests()
    aioclient_mock.put(
        f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True, "mdblist": True}
    )
    aioclient_mock.get(MOTN_CHECK, json=[{"id": "action", "name": "Action"}])
    aioclient_mock.get(TMDB_CHECK, json={"movie_results": []})
    aioclient_mock.get(MDBLIST_CHECK, json={"api_requests": 1000})
    result = await hass.config_entries.options.async_configure(
        result["flow_id"],
        {"motn_api_key": MOTN, "tmdb_read_access_token": TMDB_KEY, "mdblist_api_key": MDBLIST},
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    assert config.options == {
        "motn_api_key": MOTN,
        "tmdb_read_access_token": TMDB_KEY,
        "mdblist_api_key": MDBLIST,
    }
    assert key_pushes(aioclient_mock) == [{"motn": MOTN, "tmdb": TMDB_KEY, "mdblist": MDBLIST}]
    mdblist_checks = [
        call for call in aioclient_mock.mock_calls if call[1].host == "api.mdblist.com"
    ]
    assert [call[1].query.get("apikey") for call in mdblist_checks] == [MDBLIST]
    # The TMDB token travels as a bearer token, the Movie of the Night key in its header.
    headers = {call[1].host: call[3] for call in aioclient_mock.mock_calls if call[0] == "GET"}
    assert headers["api.themoviedb.org"]["Authorization"] == f"Bearer {TMDB_KEY}"
    assert headers["api.movieofthenight.com"]["X-API-Key"] == MOTN


async def test_a_cleared_key_is_not_sent_and_unchanged_keys_are_not_checked_again(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config, options={"motn_api_key": MOTN, "tmdb_read_access_token": TMDB_KEY}
    )
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "overlay_ready": True})
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    # Setup sends the keys Home Assistant holds.
    assert key_pushes(aioclient_mock) == [{"motn": MOTN, "tmdb": TMDB_KEY}]

    aioclient_mock.clear_requests()
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True})
    result = await keys_form(hass, config)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"motn_api_key": MOTN, "tmdb_read_access_token": ""}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    # No service was asked about the unchanged key, and the cleared one is left out.
    assert not [call for call in aioclient_mock.mock_calls if call[0] == "GET"]
    assert key_pushes(aioclient_mock) == [{"motn": MOTN}]


@pytest.mark.parametrize(
    ("held", "pushed"),
    [
        ({"motn": True, "tmdb": True, "home_assistant": True}, False),
        ({"motn": True, "tmdb": True, "mdblist": True, "home_assistant": True}, False),
        # Lost its MDBList key.
        ({"motn": True, "tmdb": True, "mdblist": False, "home_assistant": True}, True),
        ({"motn": True, "tmdb": False, "home_assistant": True}, True),
        # Changed on the TV's own QR page: Home Assistant is the source of truth.
        ({"motn": True, "tmdb": True, "home_assistant": False}, True),
        # A Luma without streaming search reports nothing and is left alone.
        (None, False),
    ],
)
async def test_the_tv_gets_the_keys_again_when_it_lost_or_replaced_them(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
    held,
    pushed,
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config,
        options={
            "motn_api_key": MOTN,
            "tmdb_read_access_token": TMDB_KEY,
            "mdblist_api_key": MDBLIST,
        },
    )
    status = {"overlay": None, "overlay_ready": True, "cameras": 0}
    if held is not None:
        status["streaming_keys"] = held
    aioclient_mock.get(f"{TV}/api/status", json=status)
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    before = len(key_pushes(aioclient_mock))
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert (len(key_pushes(aioclient_mock)) > before) is pushed


SSE = (
    "retry: 5000\n\n"
    'event: hello\ndata: {"boot":"ab","version":"2.28.0","restarted":true,"latest":2}\n\n'
    ": ping\n\n"
    'id: ab:1\nevent: log\ndata: {"seq":1,"time":1700000000000,"level":"warning",\n'
    'data: "area":"cameras","message":"camera.door: no HLS stream"}\n\n'
    'id: ab:2\nevent: metrics\ndata: {"seq":2,"area":"metrics","data":{"work_p95_ms":9.5}}\n\n'
    "event: log\ndata: not json\n\n"
)


async def test_the_event_stream_is_read_as_server_sent_events(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    from homeassistant.helpers.aiohttp_client import async_get_clientsession

    client = LumaClient(async_get_clientsession(hass), HOST, 5862, TOKEN)
    aioclient_mock.get(f"{TV}/api/events", params={"after": "ab:0"}, text=SSE)
    received = [event async for event in REAL_EVENTS(client, "ab:0")]
    assert [(event, event_id) for event, event_id, _ in received] == [
        ("hello", None),
        ("log", "ab:1"),
        ("metrics", "ab:2"),
    ]
    assert received[1][2]["message"] == "camera.door: no HLS stream"
    assert aioclient_mock.mock_calls[-1][3]["Authorization"] == f"Bearer {TOKEN}"

    aioclient_mock.clear_requests()
    aioclient_mock.get(f"{TV}/api/events", status=401, json={"error": "Pair again"})
    with pytest.raises(LumaAuthError):
        _ = [event async for event in REAL_EVENTS(client)]


def tv_events(monkeypatch: pytest.MonkeyPatch, *streams: list[tuple[str, str | None, dict]]):
    """The TV's event stream: each connection sends the next list, the last one stays open.
    Returns the cursor each connection asked for."""
    asked: list[str | None] = []

    async def events(self, after=None):
        asked.append(after)
        for event in streams[min(len(asked), len(streams)) - 1]:
            yield event
        if len(asked) >= len(streams):
            await asyncio.Event().wait()

    monkeypatch.setattr(LumaClient, "events", events)
    monkeypatch.setattr(luma_events.LumaEvents, "_follow", REAL_FOLLOW)
    monkeypatch.setattr(luma_events, "RETRY_FIRST", 0)
    return asked


HELLO = ("hello", None, {"boot": "ab", "version": "2.28.0", "uptime_s": 5, "restarted": False})
PROBLEM = (
    "log",
    "ab:7",
    {
        "seq": 7,
        "time": 1700000000000,
        "level": "error",
        "area": "overlay",
        "message": "the camera card did not open: no overlay app",
    },
)
METRICS = (
    "metrics",
    "ab:8",
    {
        "seq": 8,
        "area": "metrics",
        "data": {"work_p95_ms": 12.5, "dropped": 3, "rss_mb": 84.2, "cpu_percent": 7.5},
    },
)


async def test_tv_events_reach_the_log_sensors_bus_and_diagnostics(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    from pytest_homeassistant_custom_component.common import async_capture_events

    tv_events(monkeypatch, [HELLO, PROBLEM, METRICS])
    fired = async_capture_events(hass, "luma_event")
    config = await setup(hass, aioclient_mock)
    await hass.async_block_till_done()

    assert "Living room TV [overlay] the camera card did not open: no overlay app" in caplog.text
    record = next(r for r in caplog.records if "camera card did not open" in r.getMessage())
    assert (record.name, record.levelname) == ("custom_components.luma.tv", "ERROR")
    assert [(event.data["level"], event.data["area"]) for event in fired] == [("error", "overlay")]
    assert fired[0].data["config_entry_id"] == config.entry_id
    from homeassistant.helpers import device_registry as dr

    device = dr.async_get(hass).async_get_device(identifiers={(DOMAIN, DEVICE)})
    assert fired[0].data["device_id"] == device.id
    assert fired[0].data["time"] == "2023-11-14T22:13:20+00:00"

    assert hass.states.get("sensor.living_room_tv_frame_time").state == "12.5"
    assert hass.states.get("sensor.living_room_tv_dropped_frames").state == "3"
    assert hass.states.get("sensor.living_room_tv_memory").state == "84.2"
    assert hass.states.get("sensor.living_room_tv_cpu").state == "7.5"
    problem = hass.states.get("sensor.living_room_tv_last_problem")
    assert problem.state == "the camera card did not open: no overlay app"
    assert problem.attributes["area"] == "overlay"

    aioclient_mock.get(f"{TV}/api/diagnostics", json={"boot": "ab", "entries": []})
    diagnostics = await async_get_config_entry_diagnostics(hass, config)
    assert diagnostics["entry"]["data"]["token"] == "**REDACTED**"
    assert diagnostics["entry"]["data"]["webhook_id"] == "**REDACTED**"
    assert diagnostics["stream"]["connected"] is True
    assert diagnostics["stream"]["cursor"] == "ab:8"
    assert diagnostics["stream"]["logs"] == [PROBLEM[2]]
    assert diagnostics["tv"]["boot"] == "ab"


async def test_the_stream_resumes_from_its_cursor_and_keeps_it_across_restarts(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
    hass_storage,
) -> None:
    # The first connection drops after one entry; the sensors wait for the TV to come back.
    asked = tv_events(monkeypatch, [HELLO, PROBLEM], [])
    config = await setup(hass, aioclient_mock)
    for _ in range(5):
        await asyncio.sleep(0)
    await hass.async_block_till_done()
    assert asked == [None, "ab:7"]
    assert hass.states.get("sensor.living_room_tv_frame_time").state == "unavailable"
    # The last problem stays, even with the TV away.
    assert hass.states.get("sensor.living_room_tv_last_problem").state.startswith("the camera")

    assert await hass.config_entries.async_unload(config.entry_id)
    await hass.async_block_till_done()
    stored = hass_storage[f"luma.events.{config.entry_id}"]["data"]
    assert stored["cursor"] == "ab:7"
    assert stored["logs"] == [PROBLEM[2]]

    asked = tv_events(monkeypatch, [HELLO])
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()
    assert asked == ["ab:7"]
    problem = hass.states.get("sensor.living_room_tv_last_problem")
    assert problem.state.startswith("the camera card did not open")

    # Removing the TV removes its history.
    aioclient_mock.delete(f"{TV}/api/pair", json={"paired": False})
    assert await hass.config_entries.async_remove(config.entry_id)
    await hass.async_block_till_done()
    assert f"luma.events.{config.entry_id}" not in hass_storage


def test_an_older_tv_is_not_pushed_an_mdblist_key_it_cannot_take() -> None:
    from custom_components.luma.streaming_keys import needs_push

    only_mdblist = {"mdblist_api_key": MDBLIST}
    older = {"streaming_keys": {"motn": False, "tmdb": False, "home_assistant": False}}
    assert not needs_push(only_mdblist, older)
    newer = {
        "streaming_keys": {"motn": False, "tmdb": False, "mdblist": False, "home_assistant": False}
    }
    assert needs_push(only_mdblist, newer)
    # Keys an older TV does take are still pushed.
    assert needs_push({**only_mdblist, "motn_api_key": MOTN}, older)


def issues(hass: HomeAssistant) -> set[str]:
    return {
        issue.translation_key
        for (domain, _), issue in ir.async_get(hass).issues.items()
        if domain == DOMAIN
    }


async def test_every_request_states_the_integration_level(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    await setup(hass, aioclient_mock)
    assert aioclient_mock.mock_calls
    for call in aioclient_mock.mock_calls:
        assert call[3]["X-Luma-Integration"] == str(INTEGRATION_API)


async def test_the_device_shows_the_luma_version_and_repairs_name_the_side_to_update(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    config = entry(hass)
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    status = {"overlay": None, "overlay_ready": True, "version": "2.30.0", "api": 1}
    status["min_integration"] = INTEGRATION_API + 1
    aioclient_mock.get(f"{TV}/api/status", json=status)
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    device = dr.async_get(hass).async_get_device(identifiers={(DOMAIN, DEVICE)})
    assert device is not None
    assert device.sw_version == "2.30.0"
    assert issues(hass) == {"integration_outdated"}

    # The TV is older than this integration needs.
    monkeypatch.setattr("custom_components.luma.versions.MIN_TV_API", 2)
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    aioclient_mock.get(
        f"{TV}/api/status",
        json={"overlay": None, "version": "2.31.0", "api": 1, "min_integration": 1},
    )
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert issues(hass) == {"tv_outdated"}
    assert dr.async_get(hass).async_get(device.id).sw_version == "2.31.0"

    # Both up to date: the issues clear themselves, and go with the entry.
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    aioclient_mock.get(
        f"{TV}/api/status",
        json={"overlay": None, "version": "2.32.0", "api": 2, "min_integration": 1},
    )
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=2, seconds=2))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert issues(hass) == set()


async def test_a_tv_without_an_endpoint_asks_for_a_luma_update_until_luma_changes(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = entry(hass)
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    hass.config_entries.async_update_entry(config, options={"motn_api_key": MOTN})
    # A Luma from before streaming search and before its status reported a version.
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "api": 1})
    aioclient_mock.put(f"{TV}/api/streaming-keys", status=404, json={"error": "Unknown endpoint"})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    assert issues(hass) == {"tv_outdated"}

    # The same Luma, polled again: still outdated.
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert issues(hass) == {"tv_outdated"}

    # Luma was updated.
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    aioclient_mock.get(
        f"{TV}/api/status",
        json={"overlay": None, "version": "2.30.0", "api": 1, "min_integration": 1},
    )
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=2, seconds=2))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert issues(hass) == set()

    aioclient_mock.delete(f"{TV}/api/pair", json={"paired": False})
    aioclient_mock.put(f"{TV}/api/streaming-keys", status=404, json={})
    config.runtime_data.compatibility.missing_endpoint()
    assert issues(hass) == {"tv_outdated"}
    await hass.config_entries.async_remove(config.entry_id)
    assert issues(hass) == set()


async def test_a_steady_stream_does_not_postpone_saving_the_cursor(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
    hass_storage,
) -> None:
    # A summary a minute would keep restarting a delayed save; the first entry's save stands.
    tv_events(monkeypatch, [HELLO, PROBLEM])
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    config = await setup(hass, aioclient_mock)
    await hass.async_block_till_done()
    events = config.runtime_data.events
    for minute in range(1, 6):
        events._handle("metrics", f"ab:{100 + minute}", {"seq": 100 + minute, "data": {}})
        async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=minute))
        await hass.async_block_till_done()
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=luma_events.SAVE_DELAY + 1))
    await hass.async_block_till_done()
    stored = hass_storage[f"luma.events.{config.entry_id}"]["data"]
    assert stored["cursor"] is not None
    assert stored["logs"] == [PROBLEM[2]]


def keyboard_calls(aioclient_mock: AiohttpClientMocker, path: str) -> list:
    return [call[2] for call in aioclient_mock.mock_calls if call[1].path == path]


async def test_tv_keyboard_entity_types_into_the_open_field(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_client_no_auth,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    config = await setup(hass, aioclient_mock)
    webhook_id = config.data["webhook_id"]
    from homeassistant.helpers import entity_registry as er

    registry = er.async_get(hass)
    assert registry.async_get_entity_id("text", DOMAIN, f"{DEVICE}_keyboard") is None

    async def phone_keyboard(form: dict) -> None:
        result = await hass.config_entries.options.async_init(config.entry_id)
        result = await hass.config_entries.options.async_configure(
            result["flow_id"], {"next_step_id": "phone_keyboard"}
        )
        result = await hass.config_entries.options.async_configure(result["flow_id"], form)
        assert result["type"] is FlowResultType.CREATE_ENTRY
        await hass.async_block_till_done(wait_background_tasks=True)

    # On without any phone: the TV is asked to report its fields, and the entity appears.
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    await phone_keyboard({"keyboard_phones": [], "keyboard_entity": True})
    assert {"webhook": f"http://192.168.1.10:8123/api/webhook/{webhook_id}"} in keyboard_calls(
        aioclient_mock, "/api/keyboard"
    )
    entity_id = registry.async_get_entity_id("text", DOMAIN, f"{DEVICE}_keyboard")
    assert hass.states.get(entity_id).state == "unavailable"  # no field open

    client = await hass_client_no_auth()
    await client.post(f"/api/webhook/{webhook_id}", json={"open": True, "field": "search"})
    await hass.async_block_till_done()
    state = hass.states.get(entity_id)
    assert state.state == ""
    assert state.attributes["field"] == "search" and state.attributes["prompt"] == "Search"

    aioclient_mock.post(f"{TV}/api/keyboard/text", json={"typed": True})
    await hass.services.async_call(
        "text", "set_value", {"entity_id": entity_id, "value": " Dune\nTwo "}, blocking=True
    )
    assert keyboard_calls(aioclient_mock, "/api/keyboard/text") == [{"text": "Dune Two"}]

    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/keyboard/text", status=409, json={"error": "No field"})
    with pytest.raises(HomeAssistantError, match="No text field is open"):
        await hass.services.async_call(
            "text", "set_value", {"entity_id": entity_id, "value": "Alien"}, blocking=True
        )

    await client.post(f"/api/webhook/{webhook_id}", json={"open": False})
    await hass.async_block_till_done()
    assert hass.states.get(entity_id).state == "unavailable"

    # A TV that no longer knows this server asks to pair again instead of "unreachable".
    await client.post(f"/api/webhook/{webhook_id}", json={"open": True, "field": "search"})
    await hass.async_block_till_done()
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/keyboard/text", status=401, json={"error": "Pair again"})
    with pytest.raises(HomeAssistantError, match="pair again"):
        await hass.services.async_call(
            "text", "set_value", {"entity_id": entity_id, "value": "Alien"}, blocking=True
        )
    await hass.async_block_till_done()
    flows = hass.config_entries.flow.async_progress_by_handler(DOMAIN)
    assert [flow["context"]["source"] for flow in flows] == ["reauth"]

    # A TV that drops off with a field open stops offering the keyboard at the next check.
    aioclient_mock.clear_requests()
    aioclient_mock.get(f"{TV}/api/status", exc=TimeoutError)
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=2))
    await hass.async_block_till_done()
    assert hass.states.get(entity_id).state == "unavailable"

    # Back with the hook still held and the field still open: the field is read back.
    aioclient_mock.clear_requests()
    aioclient_mock.get(
        f"{TV}/api/status", json={"overlay": None, "cameras": 0, "keyboard_hook": True}
    )
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": "search"})
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=4))
    await hass.async_block_till_done()
    assert hass.states.get(entity_id).attributes["field"] == "search"

    # A TV that answers with an error is still there: its field stays.
    aioclient_mock.clear_requests()
    aioclient_mock.get(f"{TV}/api/status", status=500, json={"error": "Busy"})
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=6))
    await hass.async_block_till_done()
    assert hass.states.get(entity_id).attributes["field"] == "search"

    # Off again: the TV stops reporting and the entity goes from the registry.
    aioclient_mock.clear_requests()
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "overlay_ready": True})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    await phone_keyboard({"keyboard_phones": [], "keyboard_entity": False})
    assert {} in keyboard_calls(aioclient_mock, "/api/keyboard")
    assert registry.async_get_entity_id("text", DOMAIN, f"{DEVICE}_keyboard") is None


async def test_phones_chosen_in_options_are_offered_typing_when_a_tv_field_opens(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_client_no_auth,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    pixel = async_mock_service(hass, "notify", "mobile_app_pixel")
    async_mock_service(hass, "notify", "mobile_app_ipad")
    async_mock_service(hass, "notify", "persistent_notification")
    MockConfigEntry(domain="mobile_app", title="Pixel", data={"device_name": "Pixel"}).add_to_hass(
        hass
    )
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    config = await setup(hass, aioclient_mock)
    webhook_id = config.data["webhook_id"]
    # Off until a phone is chosen: the TV is not asked to report anything.
    assert keyboard_calls(aioclient_mock, "/api/keyboard") == []

    result = await hass.config_entries.options.async_init(config.entry_id)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"next_step_id": "phone_keyboard"}
    )
    assert result["step_id"] == "phone_keyboard"
    selector = next(iter(result["data_schema"].schema.values()))
    assert selector.config["options"] == [
        {"value": "mobile_app_ipad", "label": "ipad"},
        {"value": "mobile_app_pixel", "label": "Pixel"},
    ]
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"keyboard_phones": ["mobile_app_pixel"]}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    assert keyboard_calls(aioclient_mock, "/api/keyboard") == [
        {"webhook": f"http://192.168.1.10:8123/api/webhook/{webhook_id}"}
    ]

    # The TV opens its search field: the chosen phone gets a notification with a reply box.
    client = await hass_client_no_auth()
    response = await client.post(
        f"/api/webhook/{webhook_id}", json={"open": True, "field": "search"}
    )
    assert response.status == 200
    await hass.async_block_till_done()
    assert len(pixel) == 1
    data = pixel[0].data
    assert data["title"] == "Living room TV"
    action = data["data"]["actions"][0]
    assert action["behavior"] == "textInput"
    assert action["textInputPlaceholder"] == "Search"
    assert data["data"]["tag"] == f"luma_keyboard_{config.entry_id}"
    # A tap opens the typing page for this TV, on iPhone and on Android.
    assert data["data"]["url"] == f"/luma-keyboard?entry={config.entry_id}"
    assert data["data"]["clickAction"] == data["data"]["url"]

    # The reply is typed on the TV; another entry's or a blank reply is not.
    aioclient_mock.post(f"{TV}/api/keyboard/text", json={"typed": True})
    for reply in [
        {"action": "LUMA_KEYBOARD_other", "reply_text": "Alien"},
        {"action": action["action"], "reply_text": "  "},
        {"action": action["action"], "reply_text": " Dune\nPart\tTwo "},
    ]:
        hass.bus.async_fire("mobile_app_notification_action", reply)
    await hass.async_block_till_done()
    # Line breaks (Enter on Android) become spaces: the TV's fields are one line.
    assert keyboard_calls(aioclient_mock, "/api/keyboard/text") == [{"text": "Dune Part Two"}]

    # The field closes: the notification is cleared.
    await client.post(f"/api/webhook/{webhook_id}", json={"open": False})
    await hass.async_block_till_done()
    assert pixel[-1].data == {
        "message": "clear_notification",
        "data": {"tag": f"luma_keyboard_{config.entry_id}"},
    }

    # A TV that restarted lost the webhook and gets it again.
    aioclient_mock.clear_requests()
    aioclient_mock.get(
        f"{TV}/api/status", json={"overlay": None, "cameras": 0, "keyboard_hook": False}
    )
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert len(keyboard_calls(aioclient_mock, "/api/keyboard")) == 1

    # Choosing no phones tells the TV to stop calling.
    aioclient_mock.clear_requests()
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    result = await hass.config_entries.options.async_init(config.entry_id)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"next_step_id": "phone_keyboard"}
    )
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"keyboard_phones": []}
    )
    await hass.async_block_till_done()
    assert keyboard_calls(aioclient_mock, "/api/keyboard") == [{}]


async def test_the_live_view_relays_a_cameras_video_from_its_last_keyframe(
    hass: HomeAssistant, hass_client_no_auth, monkeypatch: pytest.MonkeyPatch
) -> None:
    import json
    import struct
    import threading

    from homeassistant.setup import async_setup_component

    from custom_components.luma import relay

    assert await async_setup_component(hass, "http", {})
    assert await async_setup_component(hass, DOMAIN, {})

    class Camera:
        def __init__(self, source: str | None) -> None:
            self.access_tokens = ["old", "new"]
            self.stream_options = {"rtsp_transport": "tcp"}
            self.source = source

        async def stream_source(self) -> str | None:
            return self.source

    cameras = {
        "camera.door": Camera("rtsp://nvr/door"),
        "camera.still": Camera(None),
        "camera.broken": Camera("rtsp://nvr/broken"),
    }
    monkeypatch.setattr("custom_components.luma._camera", lambda _hass, entity: cameras.get(entity))
    opened: list[tuple[str, dict[str, str]]] = []
    more = threading.Event()
    finish = threading.Event()
    key = b"\x00\x00\x00\x01\x67sps\x00\x00\x00\x01\x65idr"

    def reader(source: str, options: dict[str, str]):
        opened.append((source, options))
        if source.endswith("broken"):
            raise ValueError("no route to camera")
        yield "h264", 1920, 1080
        # Before anyone listens: an old keyframe group, then the one a new listener starts from.
        yield relay.Frame(b"old-key", 0, True)
        yield relay.Frame(b"old-next", 33_000, False)
        yield relay.Frame(key, 66_000, True)
        yield relay.Frame(b"p1", 99_000, False)
        more.wait(5)
        yield relay.Frame(b"p2", 132_000, False)
        finish.wait(30)

    monkeypatch.setattr(relay, "read_source", reader)
    hass.data.pop("luma_relays", None)
    monkeypatch.setattr("custom_components.luma.Relays", lambda hass_: relay.Relays(hass_, reader))
    client = await hass_client_no_auth()

    assert (await client.get("/api/luma/live/camera.door?token=guess")).status == 403
    assert (await client.get("/api/luma/live/camera.gone?token=new")).status == 404
    assert (await client.get("/api/luma/live/camera.still?token=new")).status == 404
    assert (await client.get("/api/luma/live/camera.broken?token=new")).status == 502

    # Warming starts the relay without sending video.
    warm = await client.get("/api/luma/live/camera.door?token=new&warm=1")
    assert await warm.json() == {"warm": True}
    live = hass.data["luma_relays"].relays["camera.door"]
    for _ in range(100):
        if len(live.kept.frames) == 2:
            break
        await asyncio.sleep(0.01)
    assert [frame.data for frame in live.kept.frames] == [key, b"p1"]

    response = await client.get("/api/luma/live/camera.door?token=old")
    assert response.status == 200

    async def frame() -> tuple[int, int, bytes]:
        length, flags, pts = struct.unpack(">IBq", await response.content.readexactly(13))
        return flags, pts, await response.content.readexactly(length)

    assert await response.content.readexactly(8) == b"LUMAVID1"
    (length,) = struct.unpack(">I", await response.content.readexactly(4))
    assert json.loads(await response.content.readexactly(length)) == {
        "codec": "h264",
        "width": 1920,
        "height": 1080,
    }
    # What the camera sent since its last keyframe, marked as earlier, then live frames.
    assert await frame() == (relay.KEYFRAME | relay.CACHED, 66_000, key)
    assert await frame() == (relay.CACHED, 99_000, b"p1")
    more.set()
    assert await frame() == (0, 132_000, b"p2")
    # One reader served the warm-up and the listener, with the camera's own stream options.
    assert opened.count(("rtsp://nvr/door", opened[-1][1])) == 1
    assert opened[-1][1]["rtsp_transport"] == "tcp"
    assert opened[-1][1]["rtsp_flags"] == "prefer_tcp"
    # Cameras and recorders use self-signed certificates, as Home Assistant's own stream allows.
    assert opened[-1][1]["tls_verify"] == "0"
    response.close()
    hass.data["luma_relays"].stop()
    finish.set()
    for _ in range(200):
        if not any(thread.name.startswith("luma-relay") for thread in threading.enumerate()):
            break
        await asyncio.sleep(0.01)


def test_relayed_keyframes_carry_their_parameter_sets() -> None:
    from custom_components.luma import relay

    sps_idr = b"\x00\x00\x00\x01\x67a\x00\x00\x00\x01\x68b\x00\x00\x01\x65c"
    assert relay.nal_types(sps_idr, "h264") == [7, 8, 5]
    assert relay.has_parameter_sets(sps_idr, "h264")
    assert not relay.has_parameter_sets(b"\x00\x00\x00\x01\x65idr", "h264")
    hevc = b"\x00\x00\x01\x40v\x00\x00\x01\x42s\x00\x00\x01\x44p\x00\x00\x01\x26i"
    assert relay.nal_types(hevc, "hevc") == [32, 33, 34, 19]
    assert relay.has_parameter_sets(hevc, "hevc")
    assert relay.annex_b(b"\x00\x00\x00\x01\x67") == b"\x00\x00\x00\x01\x67"
    # MP4-style extradata is not Annex B: those streams go through a bitstream filter.
    assert relay.annex_b(b"\x01\x64\x00\x1f") is None
    # A listener too far behind drops what is queued and resumes at the next keyframe.
    kept = relay.Kept()
    listener = relay.Listener(kept)
    kept.listeners.add(listener)
    kept.add(relay.Frame(b"k", 0, True))
    for index in range(relay.MAX_QUEUED + 5):
        kept.add(relay.Frame(b"p", index, False))
    assert listener.queue.qsize() < 10 and listener.waiting
    kept.add(relay.Frame(b"k2", 9, True))
    assert not listener.waiting


async def test_tapping_the_notification_opens_a_page_that_types_on_the_tv(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    hass_client_no_auth,
    hass_ws_client,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from homeassistant.setup import async_setup_component

    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    hass.config.internal_url = "http://192.168.1.10:8123"
    # Home Assistant's frontend itself is not installed for tests; panels need only its registry.
    assert await async_setup_component(hass, "websocket_api", {})
    hass.config.components.add("frontend")
    async_mock_service(hass, "notify", "mobile_app_pixel")
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config, options={"keyboard_phones": ["mobile_app_pixel"]}
    )
    aioclient_mock.get(f"{TV}/api/status", json={"overlay": None, "overlay_ready": True})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()

    # The page is a panel outside the sidebar, for every user, with its script served.
    panel = hass.data["frontend_panels"]["luma-keyboard"]
    assert panel.sidebar_title is None and not panel.require_admin
    module = panel.config["_panel_custom"]["module_url"]
    assert panel.config["_panel_custom"]["name"] == "luma-keyboard-panel"
    client = await hass_client_no_auth()
    script = await client.get(module)
    assert script.status == 200
    assert "customElements.define('luma-keyboard-panel'" in await script.text()

    ws = await hass_ws_client(hass)

    async def call(**message):
        await ws.send_json_auto_id(message)
        return await ws.receive_json()

    tv = {"entry_id": config.entry_id, "title": "Living room TV"}
    assert (await call(type="luma/keyboard/tvs"))["result"] == {"tvs": [{**tv, "prompt": None}]}
    await client.post(
        f"/api/webhook/{config.data['webhook_id']}", json={"open": True, "field": "titles"}
    )
    assert (await call(type="luma/keyboard/tvs"))["result"] == {
        "tvs": [{**tv, "prompt": "Movies & shows"}]
    }

    aioclient_mock.post(f"{TV}/api/keyboard/text", json={"typed": True})
    reply = await call(type="luma/keyboard/type", entry_id=config.entry_id, text=" Dune\nTwo ")
    assert reply["success"]
    assert keyboard_calls(aioclient_mock, "/api/keyboard/text") == [{"text": "Dune Two"}]
    reply = await call(type="luma/keyboard/type", entry_id=config.entry_id, text="  ")
    assert reply["error"]["code"] == "invalid_format"
    reply = await call(type="luma/keyboard/type", entry_id="other", text="Alien")
    assert reply["error"]["code"] == "not_found"

    # The field closed on the TV before the text arrived.
    await client.post(f"/api/webhook/{config.data['webhook_id']}", json={"open": False})
    assert (await call(type="luma/keyboard/tvs"))["result"] == {"tvs": [{**tv, "prompt": None}]}
    aioclient_mock.clear_requests()
    aioclient_mock.post(f"{TV}/api/keyboard/text", status=409, json={"error": "No field"})
    reply = await call(type="luma/keyboard/type", entry_id=config.entry_id, text="Alien")
    assert reply["error"]["code"] == "no_field"

    # A TV that restarted with a field open never reported it closing: registering the webhook
    # again says what is open now.
    await client.post(
        f"/api/webhook/{config.data['webhook_id']}", json={"open": True, "field": "search"}
    )
    aioclient_mock.get(
        f"{TV}/api/status", json={"overlay": None, "cameras": 0, "keyboard_hook": False}
    )
    aioclient_mock.put(f"{TV}/api/keyboard", json={"keyboard": None})
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert (await call(type="luma/keyboard/tvs"))["result"] == {"tvs": [{**tv, "prompt": None}]}


def account_calls(aioclient_mock: AiohttpClientMocker) -> list:
    return [
        (call[0], call[2])
        for call in aioclient_mock.mock_calls
        if call[1].path == "/api/criterion-account"
    ]


async def account_form(hass: HomeAssistant, config: MockConfigEntry):
    result = await hass.config_entries.options.async_init(config.entry_id)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"next_step_id": "criterion_account"}
    )
    assert result["step_id"] == "criterion_account"
    return result


async def test_the_criterion_account_is_checked_sent_and_never_in_diagnostics(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    from custom_components.luma.criterion_account import fingerprint

    config = await setup(hass, aioclient_mock)
    aioclient_mock.put(f"{TV}/api/criterion-account", json={"criterion_account": True})
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={})

    result = await account_form(hass, config)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"criterion_email": "me@example.com"}
    )
    assert result["errors"] == {"base": "criterion_pair"}
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"criterion_email": "not an email", "criterion_password": "pw"}
    )
    assert result["errors"] == {"criterion_email": "criterion_email"}
    assert account_calls(aioclient_mock) == []

    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"criterion_email": " me@example.com ", "criterion_password": "pw 1"}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    assert config.options["criterion_email"] == "me@example.com"
    mark = fingerprint(config.entry_id, "me@example.com", "pw 1")
    assert len(mark) == 64 and "pw" not in mark
    assert account_calls(aioclient_mock) == [
        ("PUT", {"email": "me@example.com", "password": "pw 1", "fingerprint": mark})
    ]

    aioclient_mock.get(f"{TV}/api/diagnostics", json={"boot": "ab", "entries": []})
    diagnostics = await async_get_config_entry_diagnostics(hass, config)
    for key in ("criterion_email", "criterion_password"):
        assert diagnostics["entry"]["options"][key] == "**REDACTED**"

    # Both fields cleared: nothing is sent now, and what was cleared is remembered.
    aioclient_mock.clear_requests()
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={})
    result = await account_form(hass, config)
    result = await hass.config_entries.options.async_configure(
        result["flow_id"], {"criterion_email": "", "criterion_password": ""}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    assert account_calls(aioclient_mock) == []
    assert config.options["criterion_cleared"] == mark


def test_only_the_account_cleared_here_is_forgotten() -> None:
    from custom_components.luma.criterion_account import fingerprint, needs_forget, saved

    mine = fingerprint("entry", "me@example.com", "pw")
    held = {"criterion_email": "me@example.com", "criterion_password": "pw"}
    cleared = saved("entry", held, "", "")
    assert cleared["criterion_cleared"] == mine
    assert needs_forget(cleared, {"criterion_account": mine})
    # Another Home Assistant server's account, or none: left alone.
    assert not needs_forget(cleared, {"criterion_account": "theirs"})
    assert not needs_forget(cleared, {"criterion_account": False})
    # A server that never held one forgets nothing.
    assert not needs_forget({}, {"criterion_account": mine})
    # Saving the empty form again keeps the record; setting an account drops it.
    assert saved("entry", cleared, "", "")["criterion_cleared"] == mine
    assert "criterion_cleared" not in saved("entry", cleared, "a@b.c", "x")


@pytest.mark.parametrize(
    ("held", "action"),
    [
        ("mine", None),
        # Reset, or changed here while the TV was off.
        (False, "PUT"),
        ("old", "PUT"),
        # An older Luma reports nothing.
        ("absent", None),
    ],
)
async def test_the_tv_gets_the_criterion_account_whenever_it_holds_another(
    hass: HomeAssistant,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
    held,
    action,
) -> None:
    from custom_components.luma.criterion_account import fingerprint

    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    config = entry(hass)
    hass.config_entries.async_update_entry(
        config, options={"criterion_email": "me@example.com", "criterion_password": "pw"}
    )
    status = {"overlay": None, "overlay_ready": True, "cameras": 0}
    if held != "absent":
        status["criterion_account"] = (
            fingerprint(config.entry_id, "me@example.com", "pw") if held == "mine" else held
        )
    aioclient_mock.get(f"{TV}/api/status", json=status)
    aioclient_mock.put(f"{TV}/api/criterion-account", json={"criterion_account": True})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    # Setup sends it once, whatever the TV holds.
    assert [method for method, _ in account_calls(aioclient_mock)] == ["PUT"]
    aioclient_mock.clear_requests()
    aioclient_mock.get(f"{TV}/api/status", json=status)
    aioclient_mock.put(f"{TV}/api/criterion-account", json={"criterion_account": True})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert [method for method, _ in account_calls(aioclient_mock)] == ([action] if action else [])


async def test_a_cleared_criterion_account_leaves_the_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("custom_components.luma.CAMERAS_PUSH_DELAY", 0)
    config = entry(hass)
    hass.config_entries.async_update_entry(config, options={"criterion_cleared": "f00d"})
    status = {"overlay": None, "overlay_ready": True, "cameras": 0, "criterion_account": "f00d"}
    aioclient_mock.get(f"{TV}/api/status", json=status)
    aioclient_mock.delete(f"{TV}/api/criterion-account", json={"criterion_account": False})
    aioclient_mock.post(f"{TV}/api/cameras", json={"cameras": 0})
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(minutes=1, seconds=1))
    await hass.async_block_till_done(wait_background_tasks=True)
    assert ("DELETE", None) in account_calls(aioclient_mock)
