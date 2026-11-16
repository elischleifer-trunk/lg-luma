"""Buttons on the TV's device: close the camera card, or preview what the overlays look like."""

from __future__ import annotations

from homeassistant.components.button import ButtonEntity
from homeassistant.core import HomeAssistant, callback
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from . import EXAMPLE_IMAGE_URL, LumaConfigEntry, dev_access, local_url
from .api import LumaAuthError, LumaError
from .entity import LumaEntity

EXAMPLE_TOAST = "Example from Home Assistant: the washer is done"


async def async_setup_entry(
    hass: HomeAssistant, entry: LumaConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    entities: list[ButtonEntity] = [
        LumaDismissButton(entry),
        LumaExampleCameraButton(entry),
        LumaExampleToastButton(entry),
    ]
    if entry.runtime_data.dev_entities:
        entities += [
            LumaDevDecisionButton(entry, approve=True),
            LumaDevDecisionButton(entry, approve=False),
            LumaDevRevokeButton(entry),
        ]
    async_add_entities(entities)


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


class LumaDevButton(LumaEntity, ButtonEntity):
    """Developer access, from the TV's device page. Pressing needs a Home Assistant login,
    like any button, so it is as trusted as the Approve on the owner's phone."""

    def __init__(self, entry: LumaConfigEntry, key: str) -> None:
        super().__init__(entry, key)
        self._entry_id = entry.entry_id

    async def async_added_to_hass(self) -> None:
        self.async_on_remove(
            async_dispatcher_connect(self.hass, dev_access.signal(self._entry_id), self._changed)
        )

    @callback
    def _changed(self) -> None:
        self.async_write_ha_state()

    @property
    def _broker(self) -> dev_access.Broker | None:
        return dev_access.broker_for(self.hass, self._entry_id)

    @property
    def available(self) -> bool:
        return self._broker is not None


class LumaDevDecisionButton(LumaDevButton):
    """Approve or deny the oldest request still waiting; unavailable while none is."""

    def __init__(self, entry: LumaConfigEntry, *, approve: bool) -> None:
        super().__init__(entry, "dev_access_approve" if approve else "dev_access_deny")
        self._approve = approve

    @property
    def available(self) -> bool:
        broker = self._broker
        return broker is not None and broker.oldest_pending() is not None

    async def async_press(self) -> None:
        broker = self._broker
        request = broker.oldest_pending() if broker else None
        if broker is None or request is None:
            raise HomeAssistantError("No developer access request is waiting")
        broker.decide(request, approve=self._approve, by="home_assistant")


class LumaDevRevokeButton(LumaDevButton):
    """Take every session key off the TV now. Developer access itself stays on."""

    def __init__(self, entry: LumaConfigEntry) -> None:
        super().__init__(entry, "dev_access_revoke")

    async def async_press(self) -> None:
        broker = self._broker
        if broker is None:
            raise HomeAssistantError("Developer access is not running")
        try:
            await broker.revoke_all()
        except Exception as err:
            raise HomeAssistantError(f"Could not reach the TV: {err}") from err
