"""Home Assistant tests for custom_components/luma (pytest-homeassistant-custom-component)."""

import pathlib
import sys

import pytest

# custom_components/ lives at the repository root.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))


@pytest.fixture(autouse=True)
def auto_enable_custom_integrations(enable_custom_integrations):
    yield


@pytest.fixture(autouse=True)
def silent_tv_events(monkeypatch):
    """No event stream: it would keep a background task running, which tests that wait for
    background tasks would wait on forever. Tests of the stream put it back."""
    from custom_components.luma.events import LumaEvents

    async def follow(self):
        return

    monkeypatch.setattr(LumaEvents, "_follow", follow)
