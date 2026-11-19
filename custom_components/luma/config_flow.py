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
    BooleanSelector,
    NumberSelector,
    NumberSelectorConfig,
    NumberSelectorMode,
    SelectOptionDict,
    SelectSelector,
    SelectSelectorConfig,
    SelectSelectorMode,
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)

try:  # Home Assistant 2025.1+
    from homeassistant.helpers.service_info.ssdp import SsdpServiceInfo
except ImportError:  # pragma: no cover - older releases
    from homeassistant.components.ssdp import SsdpServiceInfo

from . import criterion_account, dev_access, dev_keys, keyboard, streaming_keys
from .api import LumaApiError, LumaClient, LumaConnectionError, LumaError
from .const import (
    CONF_CRITERION_EMAIL,
    CONF_CRITERION_PASSWORD,
    CONF_DEV_ACCESS,
    CONF_DEV_CF_ID,
    CONF_DEV_CF_SECRET,
    CONF_DEV_KEY,
    CONF_DEV_PHONE,
    CONF_DEV_SSH_PORT,
    CONF_DEV_TOKEN_HASH,
    CONF_DEV_TUNNEL,
    CONF_DEVICE_ID,
    CONF_KEYBOARD_ENTITY,
    CONF_KEYBOARD_PHONES,
    CONF_MDBLIST_KEY,
    CONF_MOTN_KEY,
    CONF_TMDB_TOKEN,
    CONF_TOKEN,
    DEFAULT_PORT,
    DOMAIN,
    WEBOSTV_DOMAIN,
)

CONF_CODE = "code"
CONF_ROTATE_TOKEN = "dev_access_rotate_token"
CONF_FORGET_HOST_KEY = "dev_access_forget_host_key"


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
            # Merge: entry.data also holds the webhook ID and developer access keys.
            return self.async_update_reload_and_abort(self._get_reauth_entry(), data_updates=data)
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
    """Streaming search keys, phone typing and developer access. Saving sends keys and phone
    typing to the TV and starts or stops developer access (see __init__.py)."""

    def __init__(self) -> None:
        self._pending: dict[str, Any] = {}
        self._token = ""

    async def async_step_init(self, user_input: dict[str, Any] | None = None) -> ConfigFlowResult:
        return self.async_show_menu(
            step_id="init",
            menu_options=[
                "phone_keyboard",
                "streaming_keys",
                "criterion_account",
                "developer_access",
            ],
        )

    async def async_step_criterion_account(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """The account the TV's Criterion player signs in with. Both empty removes it."""
        errors: dict[str, str] = {}
        current = self.config_entry.options
        if user_input is not None:
            email = (user_input.get(CONF_CRITERION_EMAIL) or "").strip()
            password = user_input.get(CONF_CRITERION_PASSWORD) or ""
            errors = criterion_account.check(email, password)
            if not errors:
                return self.async_create_entry(
                    data=criterion_account.saved(
                        self.config_entry.entry_id, dict(current), email, password
                    )
                )
        values = user_input or current
        return self.async_show_form(
            step_id="criterion_account",
            data_schema=vol.Schema(
                {
                    vol.Optional(
                        CONF_CRITERION_EMAIL,
                        description={"suggested_value": values.get(CONF_CRITERION_EMAIL, "")},
                    ): TextSelector(TextSelectorConfig(type=TextSelectorType.EMAIL)),
                    vol.Optional(
                        CONF_CRITERION_PASSWORD,
                        description={"suggested_value": values.get(CONF_CRITERION_PASSWORD, "")},
                    ): SECRET,
                }
            ),
            errors=errors,
        )

    async def async_step_developer_access(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Off by default. Turning it on (or making a new token) shows the dev token once."""
        entry = self.config_entry
        current = entry.options
        errors: dict[str, str] = {}
        if user_input is not None:
            on = bool(user_input.get(CONF_DEV_ACCESS))
            phone = (user_input.get(CONF_DEV_PHONE) or "").strip()
            cf_id = (user_input.get(CONF_DEV_CF_ID) or "").strip()
            cf_secret = (user_input.get(CONF_DEV_CF_SECRET) or "").strip()
            if on and not phone:
                errors[CONF_DEV_PHONE] = "phone_required"
            if bool(cf_id) != bool(cf_secret):
                errors["base"] = "cloudflare_pair"
            if not errors:
                self._pending = {
                    **current,
                    CONF_DEV_ACCESS: on,
                    CONF_DEV_PHONE: phone,
                    CONF_DEV_SSH_PORT: int(user_input.get(CONF_DEV_SSH_PORT) or 22),
                    CONF_DEV_TUNNEL: (user_input.get(CONF_DEV_TUNNEL) or "").strip(),
                    CONF_DEV_CF_ID: cf_id,
                    CONF_DEV_CF_SECRET: cf_secret,
                }
                if user_input.get(CONF_FORGET_HOST_KEY):
                    await dev_access.async_stop(self.hass, entry)
                    await dev_access.forget_host_key(self.hass, entry)
                    # Start again now: unchanged options would not call the update listener.
                    await dev_access.async_update(self.hass, entry)
                new_token = on and (
                    user_input.get(CONF_ROTATE_TOKEN) or not entry.data.get(CONF_DEV_TOKEN_HASH)
                )
                if (on and not entry.data.get(CONF_DEV_KEY)) or new_token:
                    data = dict(entry.data)
                    if not data.get(CONF_DEV_KEY):
                        pair = await self.hass.async_add_executor_job(dev_keys.generate_key)
                        data[CONF_DEV_KEY] = pair.private_key
                    if new_token:
                        self._token = dev_keys.new_token()
                        data[CONF_DEV_TOKEN_HASH] = dev_keys.token_hash(self._token)
                    self.hass.config_entries.async_update_entry(entry, data=data)
                if self._token:
                    return await self.async_step_developer_token()
                return self.async_create_entry(data=self._pending)
        values = user_input or current
        phones = keyboard.available_phones(self.hass)
        chosen = values.get(CONF_DEV_PHONE) or ""
        options = [SelectOptionDict(value=service, label=name) for service, name in phones.items()]
        if chosen and chosen not in phones:
            options.append(SelectOptionDict(value=chosen, label=chosen))
        key = entry.data.get(CONF_DEV_KEY)
        line = (
            await self.hass.async_add_executor_job(dev_keys.manager_line, key)
            if key
            else "(made when you turn developer access on)"
        )
        return self.async_show_form(
            step_id="developer_access",
            data_schema=vol.Schema(
                {
                    vol.Optional(
                        CONF_DEV_ACCESS, default=bool(values.get(CONF_DEV_ACCESS))
                    ): BooleanSelector(),
                    vol.Optional(
                        CONF_DEV_PHONE, description={"suggested_value": chosen or None}
                    ): SelectSelector(
                        SelectSelectorConfig(options=options, mode=SelectSelectorMode.DROPDOWN)
                    ),
                    vol.Optional(
                        CONF_DEV_SSH_PORT, default=int(values.get(CONF_DEV_SSH_PORT) or 22)
                    ): NumberSelector(
                        NumberSelectorConfig(min=1, max=65535, mode=NumberSelectorMode.BOX)
                    ),
                    vol.Optional(
                        CONF_DEV_TUNNEL,
                        description={"suggested_value": values.get(CONF_DEV_TUNNEL) or None},
                    ): str,
                    vol.Optional(
                        CONF_DEV_CF_ID,
                        description={"suggested_value": values.get(CONF_DEV_CF_ID) or None},
                    ): str,
                    vol.Optional(
                        CONF_DEV_CF_SECRET,
                        description={"suggested_value": values.get(CONF_DEV_CF_SECRET) or None},
                    ): SECRET,
                    vol.Optional(CONF_ROTATE_TOKEN, default=False): BooleanSelector(),
                    vol.Optional(CONF_FORGET_HOST_KEY, default=False): BooleanSelector(),
                }
            ),
            errors=errors,
            description_placeholders={"authorized_keys_line": line},
        )

    async def async_step_developer_token(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        if user_input is not None:
            return self.async_create_entry(data=self._pending)
        line = await self.hass.async_add_executor_job(
            dev_keys.manager_line, self.config_entry.data[CONF_DEV_KEY]
        )
        return self.async_show_form(
            step_id="developer_token",
            description_placeholders={"token": self._token, "authorized_keys_line": line},
        )

    async def async_step_phone_keyboard(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        current = self.config_entry.options
        if user_input is not None:
            return self.async_create_entry(
                data={
                    **current,
                    CONF_KEYBOARD_PHONES: user_input.get(CONF_KEYBOARD_PHONES, []),
                    CONF_KEYBOARD_ENTITY: bool(user_input.get(CONF_KEYBOARD_ENTITY)),
                }
            )
        available = keyboard.available_phones(self.hass)
        chosen = list(current.get(CONF_KEYBOARD_PHONES) or [])
        # A chosen phone that was removed stays listed, so saving does not drop it silently.
        options = [
            SelectOptionDict(value=service, label=name) for service, name in available.items()
        ]
        options += [
            SelectOptionDict(value=service, label=service)
            for service in chosen
            if service not in available
        ]
        return self.async_show_form(
            step_id="phone_keyboard",
            data_schema=vol.Schema(
                {
                    vol.Optional(
                        CONF_KEYBOARD_PHONES, description={"suggested_value": chosen}
                    ): SelectSelector(
                        SelectSelectorConfig(
                            options=options, multiple=True, mode=SelectSelectorMode.LIST
                        )
                    ),
                    vol.Optional(
                        CONF_KEYBOARD_ENTITY,
                        default=bool(current.get(CONF_KEYBOARD_ENTITY)),
                    ): BooleanSelector(),
                }
            ),
        )

    async def async_step_streaming_keys(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        errors: dict[str, str] = {}
        current = self.config_entry.options
        if user_input is not None:
            motn, tmdb, mdblist = streaming_keys.keys(user_input)
            old_motn, old_tmdb, old_mdblist = streaming_keys.keys(current)
            # Only keys that changed are checked, so saving again costs no API calls.
            errors = await streaming_keys.check(
                self.hass,
                motn if motn != old_motn else "",
                tmdb if tmdb != old_tmdb else "",
                mdblist if mdblist != old_mdblist else "",
            )
            if not errors:
                return self.async_create_entry(
                    data={
                        **current,
                        CONF_MOTN_KEY: motn,
                        CONF_TMDB_TOKEN: tmdb,
                        CONF_MDBLIST_KEY: mdblist,
                    }
                )
        values = user_input or current
        return self.async_show_form(
            step_id="streaming_keys",
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
                    vol.Optional(
                        CONF_MDBLIST_KEY,
                        description={"suggested_value": values.get(CONF_MDBLIST_KEY, "")},
                    ): SECRET,
                }
            ),
            errors=errors,
            # URLs live here, not in the translation strings (hassfest rejects them there).
            description_placeholders={
                "motn_url": "https://www.movieofthenight.com/about/api",
                "tmdb_url": "https://www.themoviedb.org/settings/api",
                "mdblist_url": "https://mdblist.com/preferences/",
            },
        )
