"""Config flow: find the TV, then pair with a code that Luma shows on screen."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any
from urllib.parse import urlparse

import voluptuous as vol
from homeassistant.config_entries import ConfigEntry, ConfigFlow, ConfigFlowResult, OptionsFlow
from homeassistant.const import CONF_HOST, CONF_PORT
from homeassistant.core import callback
from homeassistant.helpers import instance_id
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.selector import (
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)

try:  # Home Assistant 2025.1+
    from homeassistant.helpers.service_info.ssdp import SsdpServiceInfo
except ImportError:  # pragma: no cover - older releases
    from homeassistant.components.ssdp import SsdpServiceInfo

from . import streaming_keys
from .api import LumaApiError, LumaClient, LumaConnectionError, LumaError
from .const import (
    CONF_DEVICE_ID,
    CONF_MOTN_KEY,
    CONF_TMDB_TOKEN,
    CONF_TOKEN,
    DEFAULT_PORT,
    DOMAIN,
    WEBOSTV_DOMAIN,
)

CONF_CODE = "code"


class LumaConfigFlow(ConfigFlow, domain=DOMAIN):
    """Add a TV running Luma."""

    VERSION = 1

    def __init__(self) -> None:
        self._host: str | None = None
        self._port = DEFAULT_PORT
        self._title: str | None = None
        self._device_id: str | None = None
        self._pairing_id: str | None = None

    @staticmethod
    @callback
    def async_get_options_flow(config_entry: ConfigEntry) -> LumaOptionsFlow:
        return LumaOptionsFlow()

    def _client(self, token: str | None = None) -> LumaClient:
        assert self._host
        return LumaClient(async_get_clientsession(self.hass), self._host, self._port, token)

    def _webostv_title(self, host: str) -> str | None:
        """Reuse the name of a TV already set up with Home Assistant's LG webOS integration."""
        for entry in self.hass.config_entries.async_entries(WEBOSTV_DOMAIN):
            if entry.data.get(CONF_HOST) == host:
                return entry.title
        return None

    async def _identify(self) -> None:
        info = await self._client().info()
        self._device_id = info["id"]
        assert self._host
        self._title = self._title or self._webostv_title(self._host) or f"Luma ({self._host})"

    async def async_step_user(self, user_input: dict[str, Any] | None = None) -> ConfigFlowResult:
        errors: dict[str, str] = {}
        if user_input is not None:
            self._host = user_input[CONF_HOST].strip()
            self._port = user_input[CONF_PORT]
            try:
                await self._identify()
            except LumaError:
                errors["base"] = "cannot_connect"
            else:
                await self.async_set_unique_id(self._device_id)
                self._abort_if_unique_id_configured(
                    updates={CONF_HOST: self._host, CONF_PORT: self._port}
                )
                return await self.async_step_pair()
        # Suggest a TV that the LG webOS integration already knows about.
        known = [
            entry.data[CONF_HOST]
            for entry in self.hass.config_entries.async_entries(WEBOSTV_DOMAIN)
            if entry.data.get(CONF_HOST)
        ]
        default = (user_input or {}).get(CONF_HOST) or (known[0] if known else "")
        return self.async_show_form(
            step_id="user",
            data_schema=vol.Schema(
                {
                    vol.Required(CONF_HOST, default=default): str,
                    vol.Required(CONF_PORT, default=self._port): vol.All(
                        vol.Coerce(int), vol.Range(min=1, max=65535)
                    ),
                }
            ),
            errors=errors,
        )

    async def async_step_ssdp(self, discovery_info: SsdpServiceInfo) -> ConfigFlowResult:
        """An LG TV announced itself; offer it only when Luma answers on it."""
        host = urlparse(discovery_info.ssdp_location or "").hostname
        if not host:
            return self.async_abort(reason="not_luma")
        self._host = host
        friendly = discovery_info.upnp.get("friendlyName")
        self._title = self._webostv_title(host) or (f"Luma ({friendly})" if friendly else None)
        try:
            await self._identify()
        except LumaError:
            # Luma is not installed, closed, or its Home Assistant setting is off.
            return self.async_abort(reason="not_luma")
        await self.async_set_unique_id(self._device_id)
        self._abort_if_unique_id_configured(updates={CONF_HOST: host})
        self.context["title_placeholders"] = {"name": self._title or host}
        return await self.async_step_discovery_confirm()

    async def async_step_discovery_confirm(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        if user_input is not None:
            return await self.async_step_pair()
        return self.async_show_form(
            step_id="discovery_confirm",
            description_placeholders={"name": self._title or self._host or ""},
        )

    async def async_step_pair(self, user_input: dict[str, Any] | None = None) -> ConfigFlowResult:
        """Luma shows a six-digit code on the TV; the user types it here."""
        errors: dict[str, str] = {}
        if user_input is not None and self._pairing_id:
            try:
                token = await self._client().confirm_pairing(
                    self._pairing_id, user_input[CONF_CODE]
                )
            except LumaApiError as err:
                if err.status == 403 and "Wrong code" in str(err):
                    errors["base"] = "invalid_code"
                else:
                    # Expired or locked out: ask the TV for a new code.
                    self._pairing_id = None
                    errors["base"] = "pairing_expired"
            except LumaConnectionError:
                errors["base"] = "cannot_connect"
            else:
                return self._finish(token)
        if self._pairing_id is None:
            try:
                self._pairing_id = await self._client().start_pairing(
                    self.hass.config.location_name or "Home Assistant",
                    await instance_id.async_get(self.hass),
                )
            except LumaConnectionError:
                return self.async_abort(reason="cannot_connect")
            except LumaApiError as err:
                if err.status == 429:
                    errors["base"] = "too_soon"
                else:
                    return self.async_abort(reason="pairing_refused")
        return self.async_show_form(
            step_id="pair",
            data_schema=vol.Schema({vol.Required(CONF_CODE): str}),
            errors=errors,
            description_placeholders={"name": self._title or self._host or ""},
        )

    def _finish(self, token: str) -> ConfigFlowResult:
        data = {
            CONF_HOST: self._host,
            CONF_PORT: self._port,
            CONF_TOKEN: token,
            CONF_DEVICE_ID: self._device_id,
        }
        if self.source == "reauth":
            return self.async_update_reload_and_abort(self._get_reauth_entry(), data=data)
        return self.async_create_entry(title=self._title or f"Luma ({self._host})", data=data)

    async def async_step_reauth(self, entry_data: Mapping[str, Any]) -> ConfigFlowResult:
        """The TV forgot this server (Home Assistant was turned off in Luma): pair again."""
        self._host = entry_data[CONF_HOST]
        self._port = entry_data.get(CONF_PORT, DEFAULT_PORT)
        self._device_id = entry_data.get(CONF_DEVICE_ID)
        self._title = self._get_reauth_entry().title
        return await self.async_step_reauth_confirm()

    async def async_step_reauth_confirm(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        if user_input is not None:
            return await self.async_step_pair()
        return self.async_show_form(
            step_id="reauth_confirm",
            description_placeholders={"name": self._title or self._host or ""},
        )


SECRET = TextSelector(TextSelectorConfig(type=TextSelectorType.PASSWORD))


class LumaOptionsFlow(OptionsFlow):
    """Streaming search keys. Saving pushes them to the TV (see __init__.py)."""

    async def async_step_init(self, user_input: dict[str, Any] | None = None) -> ConfigFlowResult:
        errors: dict[str, str] = {}
        current = self.config_entry.options
        if user_input is not None:
            motn, tmdb = streaming_keys.keys(user_input)
            old_motn, old_tmdb = streaming_keys.keys(current)
            # Only keys that changed are checked, so saving again costs no API calls.
            errors = await streaming_keys.check(
                self.hass,
                motn if motn != old_motn else "",
                tmdb if tmdb != old_tmdb else "",
            )
            if not errors:
                return self.async_create_entry(
                    data={**current, CONF_MOTN_KEY: motn, CONF_TMDB_TOKEN: tmdb}
                )
        values = user_input or current
        return self.async_show_form(
            step_id="init",
            data_schema=vol.Schema(
                {
                    vol.Optional(
                        CONF_MOTN_KEY,
                        description={"suggested_value": values.get(CONF_MOTN_KEY, "")},
                    ): SECRET,
                    vol.Optional(
                        CONF_TMDB_TOKEN,
                        description={"suggested_value": values.get(CONF_TMDB_TOKEN, "")},
                    ): SECRET,
                }
            ),
            errors=errors,
            # URLs live here, not in the translation strings (hassfest rejects them there).
            description_placeholders={
                "motn_url": "https://www.movieofthenight.com/about/api",
                "tmdb_url": "https://www.themoviedb.org/settings/api",
            },
        )
