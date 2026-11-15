"""Developer access: authorized_keys lines, key pairs and dev tokens. No Home Assistant or network.

Session keys carry a comment `luma-dev:<request id>:<expiry, Unix seconds>`. Only lines with
that tag are ever removed; every other line of the TV's authorized_keys is kept byte for byte.
Dropbear (the TV's SSH server) has no expiry option, so the tag is how expired keys are found.
"""

from __future__ import annotations

import hashlib
import hmac
import re
import secrets
from dataclasses import dataclass

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

TAG = "luma-dev"
# Keys may run commands but not turn the TV into a gateway: Dropbear supports these options.
OPTIONS = "no-port-forwarding,no-agent-forwarding,no-X11-forwarding"
_TAG_RE = re.compile(rf"\s{TAG}:([A-Za-z0-9_-]+):(\d+)\s*$")


@dataclass
class KeyPair:
    private_key: str  # OpenSSH format
    public_key: str  # "ssh-ed25519 AAAA…"


def generate_key() -> KeyPair:
    """A new ed25519 key pair (cryptography ships with Home Assistant; no asyncssh needed)."""
    key = Ed25519PrivateKey.generate()
    private = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.OpenSSH,
        serialization.NoEncryption(),
    ).decode()
    return KeyPair(private, _public(key))


def public_key_of(private_key: str) -> str:
    key = serialization.load_ssh_private_key(private_key.encode(), password=None)
    return _public(key)


def _public(key) -> str:
    return (
        key.public_key()
        .public_bytes(serialization.Encoding.OpenSSH, serialization.PublicFormat.OpenSSH)
        .decode()
    )


def manager_line(private_key: str) -> str:
    """The authorized_keys line for Home Assistant's own management key."""
    algorithm, blob = public_key_of(private_key).split()[:2]
    return f"{OPTIONS} {algorithm} {blob} luma-dev-manager"


def new_token() -> str:
    """A 256-bit dev token."""
    return secrets.token_urlsafe(32)


def token_hash(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


def token_matches(token: str | None, expected_hash: str) -> bool:
    if not token:
        return False
    return hmac.compare_digest(token_hash(token), expected_hash)


def key_line(public_key: str, request_id: str, expires_at: int, *, options: bool = True) -> str:
    """The authorized_keys line for a session key. `public_key` is `type base64` (a comment, if
    any, is replaced by the tag)."""
    if not re.fullmatch(r"[A-Za-z0-9_-]+", request_id):
        raise ValueError("bad request id")
    algorithm, blob = public_key.split()[:2]
    prefix = f"{OPTIONS} " if options else ""
    return f"{prefix}{algorithm} {blob} {TAG}:{request_id}:{int(expires_at)}"


def parse_tag(line: str) -> tuple[str, int] | None:
    """The request id and expiry of a session key line, or None for any other line."""
    match = _TAG_RE.search(line)
    if not match:
        return None
    return match.group(1), int(match.group(2))


def rewrite(
    content: str,
    *,
    add: str | None = None,
    remove: frozenset[str] | set[str] = frozenset(),
    now: float | None = None,
    remove_all: bool = False,
) -> tuple[str, dict[str, int]]:
    """authorized_keys with session keys in `remove` (and, given `now`, expired ones; with
    `remove_all`, every one) dropped and `add` appended. Returns the new content and the session
    keys left, id → expiry."""
    kept: list[str] = []
    remaining: dict[str, int] = {}
    for line in content.splitlines():
        tag = parse_tag(line)
        if tag is not None:
            request_id, expires_at = tag
            expired = now is not None and expires_at <= now
            if remove_all or request_id in remove or expired:
                continue
            remaining[request_id] = expires_at
        kept.append(line)
    if add is not None:
        tag = parse_tag(add)
        if tag is None or "\n" in add:
            raise ValueError("only tagged single-line session keys can be added")
        kept.append(add)
        remaining[tag[0]] = tag[1]
    text = "\n".join(kept)
    return (text + "\n" if text else ""), remaining
