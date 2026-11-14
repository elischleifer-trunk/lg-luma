"""Diagnostic sensors from what the TV reports: how smoothly Luma draws, its memory and CPU use,
and the last problem it logged."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from homeassistant.components.sensor import (
    SensorDeviceClass,
    SensorEntity,
    SensorEntityDescription,
    SensorStateClass,
)
from homeassistant.const import PERCENTAGE, EntityCategory, UnitOfInformation, UnitOfTime
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from . import LumaConfigEntry
from .entity import LumaEntity
from .events import LumaEvents, entry_time, signal


@dataclass(frozen=True, kw_only=True)
class LumaSensorDescription(SensorEntityDescription):
    value: Callable[[LumaEvents], Any]
    attributes: Callable[[LumaEvents], dict[str, Any] | None] = lambda _events: None
    # Performance only means something while the TV is reporting; the last problem stays.
    live: bool = True


def _problem(events: LumaEvents) -> str | None:
    problem = events.last_problem
    return str(problem.get("message", ""))[:255] if problem else None


def _problem_attributes(events: LumaEvents) -> dict[str, Any] | None:
    problem = events.last_problem
    if not problem:
        return None
    return {"area": problem.get("area"), "level": problem.get("level"), "time": entry_time(problem)}


SENSORS = (
    LumaSensorDescription(
        key="frame_time",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        device_class=SensorDeviceClass.DURATION,
        state_class=SensorStateClass.MEASUREMENT,
        suggested_display_precision=1,
        value=lambda events: events.latest("work_p95_ms"),
    ),
    LumaSensorDescription(
        key="dropped_frames",
        state_class=SensorStateClass.MEASUREMENT,
        value=lambda events: events.latest("dropped"),
    ),
    LumaSensorDescription(
        key="memory",
        native_unit_of_measurement=UnitOfInformation.MEBIBYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        suggested_display_precision=0,
        value=lambda events: events.latest("rss_mb"),
    ),
    LumaSensorDescription(
        key="cpu",
        native_unit_of_measurement=PERCENTAGE,
        state_class=SensorStateClass.MEASUREMENT,
        suggested_display_precision=0,
        value=lambda events: events.latest("cpu_percent"),
    ),
    LumaSensorDescription(
        key="last_problem",
        value=_problem,
        attributes=_problem_attributes,
        live=False,
    ),
)


async def async_setup_entry(
    hass: HomeAssistant, entry: LumaConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    async_add_entities(LumaSensor(entry, description) for description in SENSORS)


class LumaSensor(LumaEntity, SensorEntity):
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_should_poll = False
    entity_description: LumaSensorDescription

    def __init__(self, entry: LumaConfigEntry, description: LumaSensorDescription) -> None:
        super().__init__(entry, description.key)
        self.entity_description = description
        self._events = entry.runtime_data.events

    async def async_added_to_hass(self) -> None:
        self.async_on_remove(
            async_dispatcher_connect(self.hass, signal(self._events.entry.entry_id), self._updated)
        )

    @callback
    def _updated(self) -> None:
        self.async_write_ha_state()

    @property
    def available(self) -> bool:
        return self._events.connected or not self.entity_description.live

    @property
    def native_value(self) -> Any:
        return self.entity_description.value(self._events)

    @property
    def extra_state_attributes(self) -> dict[str, Any] | None:
        return self.entity_description.attributes(self._events)
