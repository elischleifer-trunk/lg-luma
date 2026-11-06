"""Buttons on the TV's device: close the camera card, or preview what the overlays look like."""

from __future__ import annotations

from homeassistant.components.button import ButtonEntity
from homeassistant.core import HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from . import EXAMPLE_IMAGE_URL, LumaConfigEntry, local_url
from .api import LumaAuthError, LumaError
from .entity import LumaEntity

EXAMPLE_TOAST = "Example from Home Assistant: the washer is done"


async def async_setup_entry(
    hass: HomeAssistant, entry: LumaConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    async_add_entities(
        [LumaDismissButton(entry), LumaExampleCameraButton(entry), LumaExampleToastButton(entry)]
    )


class LumaButton(LumaEntity, ButtonEntity):
    def __init__(self, entry: LumaConfigEntry, key: str) -> None:
        super().__init__(entry, key)
        self._entry = entry
        self._client = entry.runtime_data.client

    async def _call(self, request) -> None:
        try:
            await request
        except LumaAuthError as err:
            self._entry.async_start_reauth(self.hass)
            raise HomeAssistantError("Luma no longer knows this server; pair again") from err
        except LumaError as err:
            raise HomeAssistantError(str(err)) from err


class LumaDismissButton(LumaButton):
    def __init__(self, entry: LumaConfigEntry) -> None:
        super().__init__(entry, "dismiss_overlay")

    async def async_press(self) -> None:
        await self._call(self._client.dismiss())


class LumaExampleCameraButton(LumaButton):
    """Shows a camera card with a bundled picture of a front door, so no camera is needed."""

    def __init__(self, entry: LumaConfigEntry) -> None:
        super().__init__(entry, "example_camera")

    async def async_press(self) -> None:
        payload = {
            "snapshot_url": f"{local_url(self.hass)}{EXAMPLE_IMAGE_URL}",
            "title": "Front door",
            "message": "Example: this is how a doorbell ring looks",
            "duration": 15,
            "refresh": 30,
            "position": "top-right",
            "size": "medium",
            "pause": False,
        }
        await self._call(self._client.show(payload))


class LumaExampleToastButton(LumaButton):
    def __init__(self, entry: LumaConfigEntry) -> None:
        super().__init__(entry, "example_toast")

    async def async_press(self) -> None:
        await self._call(self._client.notify(EXAMPLE_TOAST))
