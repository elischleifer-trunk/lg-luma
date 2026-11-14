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
from pytest_homeassistant_custom_component.common import MockConfigEntry, async_fire_time_changed
from pytest_homeassistant_custom_component.test_util.aiohttp import AiohttpClientMocker

try:
    from homeassistant.helpers.service_info.ssdp import SsdpServiceInfo
except ImportError:  # pragma: no cover
    from homeassistant.components.ssdp import SsdpServiceInfo

from custom_components.luma.const import DOMAIN, INTEGRATION_API

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


async def test_options_check_streaming_keys_then_send_them_to_the_tv(
    hass: HomeAssistant, aioclient_mock: AiohttpClientMocker
) -> None:
    config = await setup(hass, aioclient_mock)
    aioclient_mock.put(f"{TV}/api/streaming-keys", json={"motn": True, "tmdb": True})
    aioclient_mock.get(MOTN_CHECK, status=403, json={"message": "Not subscribed"})
    aioclient_mock.get(TMDB_CHECK, json={"movie_results": []})

    result = await hass.config_entries.options.async_init(config.entry_id)
    assert result["step_id"] == "init"
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
    result = await hass.config_entries.options.async_init(config.entry_id)
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
