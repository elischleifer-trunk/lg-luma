"""The TV keyboard: a text entity that types into the text field open on the TV.

Optional (Type on your phone → Type from Home Assistant too). It is available only while a field
is open, so a dashboard card can show a type-to-TV box just when the TV is asking for text. Its
`field` and `prompt` attributes say what the TV is asking for. Setting a value sends it to the TV,
as replying to the phone notification does.
"""

from __future__ import annotations

from typing import Any

from homeassistant.components.text import TextEntity, TextMode
from homeassistant.core import HomeAssistant, callback
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from . import LumaConfigEntry, keyboard
from .api import LumaApiError, LumaError
from .entity import LumaEntity


async def async_setup_entry(
    hass: HomeAssistant, entry: LumaConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    if entry.runtime_data.keyboard_entity:
        async_add_entities([LumaKeyboardText(entry)])


class LumaKeyboardText(LumaEntity, TextEntity):
    _attr_should_poll = False
    _attr_mode = TextMode.TEXT
    _attr_native_min = 0
    _attr_native_max = keyboard.MAX_TEXT
    # What was typed is sent and gone; the field on the TV holds it, not this entity.
    _attr_native_value = ""

    def __init__(self, entry: LumaConfigEntry) -> None:
        super().__init__(entry, "keyboard")
        self._entry = entry

    async def async_added_to_hass(self) -> None:
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass, keyboard.signal(self._entry.entry_id), self._changed
            )
        )

    @callback
    def _changed(self) -> None:
        self.async_write_ha_state()

    @property
    def available(self) -> bool:
        return self._entry.runtime_data.field is not None

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        field = self._entry.runtime_data.field
        return {"field": field, "prompt": keyboard.prompt(field)}

    async def async_set_value(self, value: str) -> None:
        text = keyboard.clean(value)
        if not text:
            return
        try:
            await self._entry.runtime_data.client.type_text(text)
        except LumaApiError as err:
            if err.status == 409:
                raise HomeAssistantError("No text field is open on the TV") from err
            raise HomeAssistantError(str(err)) from err
        except LumaError as err:
            raise HomeAssistantError(f"Could not reach the TV: {err}") from err
