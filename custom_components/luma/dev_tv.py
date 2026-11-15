"""Developer access: SSH from Home Assistant to the TV over the home network, to add, check and
remove session keys. Imported only once developer access is on and asyncssh is installed.

Home Assistant logs in with its own management key, kept only in the config entry. The TV's host
key is pinned the first time it answers; a changed host key stops every operation until the owner
chooses Forget TV host key in the options.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import time

import asyncssh

from . import dev_keys as keys

_LOGGER = logging.getLogger(__name__)
AUTHORIZED_KEYS = "/home/root/.ssh/authorized_keys"
CONNECT_TIMEOUT = 15
# A command on the TV (cat, mv, true) answers at once; longer means the connection hung.
RUN_TIMEOUT = 30


class TvError(Exception):
    """The TV could not be reached or refused the change."""


class HostKeyChanged(TvError):
    """The TV answered with a different host key than the pinned one."""


class KeyRefused(TvError):
    """The TV's SSH server refused the key (it is not in authorized_keys, or the line has
    options this Dropbear build rejects)."""


class Tv:
    """One TV. Calls are serialized so concurrent edits of authorized_keys cannot interleave."""

    def __init__(self, host: str, port: int, management_key: str, host_key: str | None) -> None:
        self.host = host
        self.port = port
        self._key = asyncssh.import_private_key(management_key)
        self.host_key = host_key  # OpenSSH public key, or None until first contact
        self._lock = asyncio.Lock()

    async def _connect(self, client_key: asyncssh.SSHKey | None = None):
        known_hosts = None
        if self.host_key:
            known_hosts = ([asyncssh.import_public_key(self.host_key)], [], [])
        try:
            conn = await asyncio.wait_for(
                asyncssh.connect(
                    self.host,
                    self.port,
                    username="root",
                    client_keys=[client_key or self._key],
                    known_hosts=known_hosts,
                    agent_path=None,
                    config=None,
                ),
                CONNECT_TIMEOUT,
            )
        except asyncssh.HostKeyNotVerifiable as err:
            raise HostKeyChanged("the TV's host key changed") from err
        except asyncssh.PermissionDenied as err:
            raise KeyRefused("the TV refused the key") from err
        except (OSError, asyncssh.Error, TimeoutError) as err:
            raise TvError(f"could not log in to the TV: {err}") from err
        if not self.host_key:
            server_key = conn.get_server_host_key()
            if server_key is not None:
                self.host_key = server_key.export_public_key().decode().strip()
                _LOGGER.info("Pinned the TV's host key")
        return conn

    async def _run(self, conn, command: str, stdin: str | None = None) -> str:
        try:
            result = await asyncio.wait_for(conn.run(command, input=stdin, check=True), RUN_TIMEOUT)
        except asyncssh.ProcessError as err:
            raise TvError(f"command failed on the TV: {err.stderr or err}") from err
        except (OSError, asyncssh.Error, TimeoutError) as err:
            raise TvError(f"lost the connection to the TV: {err}") from err
        return str(result.stdout or "")

    async def _edit(self, conn, **changes) -> dict[str, int]:
        current = await self._run(conn, f"cat {AUTHORIZED_KEYS} 2>/dev/null || true")
        new, remaining = keys.rewrite(current, **changes)
        if new != current:
            tmp = f"{AUTHORIZED_KEYS}.luma-dev"
            await self._run(
                conn,
                f"umask 077; mkdir -p /home/root/.ssh && cat > {tmp} && mv {tmp} {AUTHORIZED_KEYS}",
                stdin=new,
            )
        return remaining

    async def _accepts(self, private_key: str) -> bool:
        """Whether the TV lets this key in. Only a clean refusal answers False; a timeout or a
        dropped connection raises, so it is never mistaken for unsupported options."""
        try:
            conn = await self._connect(asyncssh.import_private_key(private_key))
        except KeyRefused:
            return False
        async with conn:
            await self._run(conn, "true")
        return True

    async def install(self, generated: keys.KeyPair, request_id: str, expires_at: int) -> None:
        """Add a session key and check that the TV accepts it. A Dropbear build that refuses
        option-prefixed lines gets a plain line instead; this is checked again on every install,
        so one failure never drops the options for later keys."""
        async with self._lock:
            try:
                for options in (True, False):
                    line = keys.key_line(
                        generated.public_key, request_id, expires_at, options=options
                    )
                    async with await self._connect() as conn:
                        await self._edit(conn, remove={request_id}, add=line)
                    if await self._accepts(generated.private_key):
                        return
                    _LOGGER.warning("The TV refused a session key (options=%s)", options)
                raise KeyRefused("the TV refused the session key")
            except BaseException:
                # Leave nothing behind for a key the session will never receive.
                with contextlib.suppress(Exception):
                    async with await self._connect() as conn:
                        await self._edit(conn, remove={request_id})
                raise

    async def remove(self, request_ids: set[str]) -> dict[str, int]:
        async with self._lock, await self._connect() as conn:
            return await self._edit(conn, remove=request_ids)

    async def sweep(self, remove: set[str] | frozenset[str] = frozenset()) -> dict[str, int]:
        """Drop expired session keys, and those in `remove` (released while the TV was away);
        returns those still valid, id → expiry."""
        async with self._lock, await self._connect() as conn:
            return await self._edit(conn, now=time.time(), remove=remove)

    async def remove_all(self) -> None:
        """Drop every session key Home Assistant added (developer access turned off)."""
        async with self._lock, await self._connect() as conn:
            await self._edit(conn, remove_all=True)
