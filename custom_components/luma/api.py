"""Client for the LAN API that Luma serves on the TV (see src/home_assistant.rs)."""

from __future__ import annotations

from typing import Any

import aiohttp

from .const import DEFAULT_PORT

TIMEOUT = aiohttp.ClientTimeout(total=10)


class LumaError(Exception):
    """The TV refused a request or could not be reached."""


class LumaConnectionError(LumaError):
    """The TV did not answer: it is off, Luma is closed, or Home Assistant is off in Luma."""


class LumaAuthError(LumaError):
    """The token is missing or no longer paired."""


class LumaApiError(LumaError):
    """Luma answered with an error."""

    def __init__(self, status: int, message: str) -> None:
        super().__init__(message)
        self.status = status


class LumaClient:
    """One TV running Luma."""

    def __init__(
        self,
        session: aiohttp.ClientSession,
        host: str,
        port: int = DEFAULT_PORT,
        token: str | None = None,
    ) -> None:
        self._session = session
        self.host = host
        self.port = port
        self.token = token

    async def _request(
        self, method: str, path: str, payload: dict[str, Any] | None = None, auth: bool = True
    ) -> dict[str, Any]:
        headers = {}
        if auth:
            if not self.token:
                raise LumaAuthError("Not paired")
            headers["Authorization"] = f"Bearer {self.token}"
        url = f"http://{self.host}:{self.port}{path}"
        try:
            async with self._session.request(
                method, url, json=payload, headers=headers, timeout=TIMEOUT
            ) as response:
                try:
                    body = await response.json(content_type=None)
                except ValueError:
                    body = None
                status = response.status
        except (aiohttp.ClientError, TimeoutError) as err:
            raise LumaConnectionError(f"Cannot reach Luma at {self.host}:{self.port}") from err
        if not isinstance(body, dict):
            body = {}
        message = str(body.get("error") or f"HTTP {status}")
        if status == 401:
            raise LumaAuthError(message)
        if status >= 400:
            raise LumaApiError(status, message)
        return body

    async def info(self) -> dict[str, Any]:
        """Identity of the TV; needs no pairing."""
        info = await self._request("GET", "/api/info", auth=False)
        if info.get("name") != "Luma" or not isinstance(info.get("id"), str):
            raise LumaConnectionError("Not a Luma TV")
        return info

    async def start_pairing(self, name: str, server: str | None = None) -> str:
        """Ask the TV to show a pairing code; returns the pairing request ID.

        `server` is this Home Assistant's instance ID: Luma uses it to give back how the
        owner arranged this server's cameras when it pairs again.
        """
        payload = {"name": name}
        if server:
            payload["server"] = server
        reply = await self._request("POST", "/api/pair", payload, auth=False)
        return str(reply["pairing_id"])

    async def confirm_pairing(self, pairing_id: str, code: str) -> str:
        """Trade the code shown on the TV for a token."""
        reply = await self._request(
            "POST",
            "/api/pair/confirm",
            {"pairing_id": pairing_id, "code": code.strip()},
            auth=False,
        )
        token = str(reply["token"])
        self.token = token
        return token

    async def unpair(self) -> None:
        await self._request("DELETE", "/api/pair")

    async def status(self) -> dict[str, Any]:
        return await self._request("GET", "/api/status")

    async def show(self, payload: dict[str, Any]) -> dict[str, Any]:
        return await self._request("POST", "/api/overlay", payload)

    async def dismiss(self) -> None:
        await self._request("DELETE", "/api/overlay")

    async def notify(self, message: str) -> None:
        await self._request("POST", "/api/notify", {"message": message})

    async def cameras(self, cameras: list[dict[str, Any]]) -> None:
        """Replace the camera list shown by Luma's Cameras app."""
        await self._request("POST", "/api/cameras", {"cameras": cameras})

    async def streaming_keys(
        self, motn: str | None, tmdb: str | None, mdblist: str | None = None
    ) -> dict[str, Any]:
        """Set the TV's streaming search keys. Empty keys are left out, so a field cleared in
        Home Assistant never erases the TV's key. Returns which keys the TV now holds."""
        payload = {
            name: key for name, key in (("motn", motn), ("tmdb", tmdb), ("mdblist", mdblist)) if key
        }
        return await self._request("PUT", "/api/streaming-keys", payload)
