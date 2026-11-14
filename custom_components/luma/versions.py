"""Keeping the TV and this integration compatible, and showing which Luma the TV runs.

Each side states a small API level, raised only when the other side must change with it (not
at every release). This integration sends its level to the TV with every request and reads the
TV's level and minimum from its status. When either side is too old, Home Assistant raises a
repair issue naming what to update; Luma also names an outdated server in its own Settings. A TV
that answers "Unknown endpoint" for something this integration needs is too old as well.
"""

from __future__ import annotations

from typing import Any

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import issue_registry as ir

from .const import CONF_DEVICE_ID, DOMAIN, INTEGRATION_API, MIN_TV_API

TV_OUTDATED = "tv_outdated"
INTEGRATION_OUTDATED = "integration_outdated"


def _level(value: Any) -> int:
    """A level from the TV; TVs from before levels were reported count as level 1."""
    return value if isinstance(value, int) and not isinstance(value, bool) else 1


def _issue_id(kind: str, entry: ConfigEntry) -> str:
    return f"{kind}_{entry.entry_id}"


@callback
def _issue(hass: HomeAssistant, entry: ConfigEntry, kind: str, raised: bool) -> None:
    issue_id = _issue_id(kind, entry)
    if raised:
        ir.async_create_issue(
            hass,
            DOMAIN,
            issue_id,
            is_fixable=False,
            severity=ir.IssueSeverity.WARNING,
            translation_key=kind,
            translation_placeholders={"name": entry.title},
        )
    else:
        ir.async_delete_issue(hass, DOMAIN, issue_id)


@callback
def sw_version(hass: HomeAssistant, entry: ConfigEntry, version: Any) -> None:
    """Show the Luma version the TV runs on its device page."""
    if not isinstance(version, str) or not version:
        return
    dr.async_get(hass).async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, entry.data[CONF_DEVICE_ID])},
        sw_version=version,
    )


class Compatibility:
    """What one TV's status says about each side's version."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry) -> None:
        self._hass = hass
        self._entry = entry
        # The TV version reported when the TV lacked an endpoint; any other version (an update)
        # clears it. TVs from before status reported a version report None.
        self._lacking: tuple[str | None] | None = None
        self._version: str | None = None

    @callback
    def status(self, status: dict[str, Any]) -> None:
        version = status.get("version") if isinstance(status.get("version"), str) else None
        self._version = version
        if self._lacking is not None and self._lacking != (version,):
            self._lacking = None
        sw_version(self._hass, self._entry, version)
        tv_too_old = _level(status.get("api")) < MIN_TV_API or self._lacking is not None
        _issue(self._hass, self._entry, TV_OUTDATED, tv_too_old)
        integration_too_old = _level(status.get("min_integration")) > INTEGRATION_API
        _issue(self._hass, self._entry, INTEGRATION_OUTDATED, integration_too_old)

    @callback
    def missing_endpoint(self) -> None:
        """The TV answered "Unknown endpoint": this Luma predates something this integration
        uses."""
        self._lacking = (self._version,)
        _issue(self._hass, self._entry, TV_OUTDATED, True)


@callback
def remove(hass: HomeAssistant, entry: ConfigEntry) -> None:
    for kind in (TV_OUTDATED, INTEGRATION_OUTDATED):
        ir.async_delete_issue(hass, DOMAIN, _issue_id(kind, entry))
