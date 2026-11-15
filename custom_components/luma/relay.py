"""Live camera video for the TV, without HLS.

Home Assistant's HLS stream offers no video until it has connected to a camera and cut a whole
segment, several seconds after the TV asks. A relay instead reads the camera's own stream (the
source Home Assistant's stream integration would read), keeps what the camera sent since its last
keyframe, and hands a TV that connects that stretch at once, then every frame as it arrives. The
TV has a picture as soon as it has decoded one keyframe.

The video is never decoded or re-encoded here: packets are passed on as the camera sent them, as
an Annex B byte stream (start codes, with the parameter sets before each keyframe), which a
hardware decoder takes directly.

Wire format, one way, over a plain HTTP response body:

    "LUMAVID1"                      magic
    u32 length, JSON                {"codec": "h264" | "hevc", "width": …, "height": …}
    then frames, each:
    u32 length, u8 flags, i64 pts   big-endian; pts in microseconds
    payload                         `length` bytes of Annex B

Flag 1 marks a keyframe. Flag 2 marks a frame from before the TV connected, to decode without
waiting; the first frame without it is live.

A relay reads on its own thread (PyAV releases the GIL while it waits on the camera) and hands
each packet, about thirty a second, to Home Assistant's event loop, which owns the kept frames
and the listeners. It stops by itself once nobody has listened for a while.
"""

from __future__ import annotations

import asyncio
import json
import logging
import struct
import threading
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from typing import Any

from homeassistant.core import HomeAssistant, callback

_LOGGER = logging.getLogger(__name__)

MAGIC = b"LUMAVID1"
KEYFRAME = 1
CACHED = 2
CODECS = {"h264": "h264", "hevc": "hevc", "h265": "hevc"}
# A relay nobody listens to stops after this long, so a camera opened again soon starts at once.
LINGER = 120.0
# The frames kept since the last keyframe may take at most this much; a camera whose keyframes
# are further apart than that is relayed without them (a TV then waits for its next keyframe).
MAX_KEPT_BYTES = 48 * 1024 * 1024
# A listener this many frames behind is slower than the camera: it skips to the next keyframe.
MAX_QUEUED = 240
# A camera that cannot be read is tried again after this long, while someone listens.
RETRY = 3.0
START = b"\x00\x00\x01"


@dataclass(frozen=True, slots=True)
class Frame:
    """One encoded picture, as Annex B."""

    data: bytes
    pts: int
    keyframe: bool

    def encode(self, cached: bool = False) -> bytes:
        flags = (KEYFRAME if self.keyframe else 0) | (CACHED if cached else 0)
        return struct.pack(">IBq", len(self.data), flags, self.pts) + self.data


def header(codec: str, width: int, height: int) -> bytes:
    """The start of a relayed stream."""
    body = json.dumps({"codec": codec, "width": width, "height": height}).encode()
    return MAGIC + struct.pack(">I", len(body)) + body


def nal_types(data: bytes, codec: str, limit: int = 8) -> list[int]:
    """The types of the first NAL units of an Annex B buffer."""
    types: list[int] = []
    at = data.find(START)
    while at >= 0 and len(types) < limit and at + 3 < len(data):
        byte = data[at + 3]
        types.append((byte >> 1) & 0x3F if codec == "hevc" else byte & 0x1F)
        at = data.find(START, at + 3)
    return types


def has_parameter_sets(data: bytes, codec: str) -> bool:
    """Whether a keyframe carries what a decoder needs to start (SPS, and VPS for HEVC)."""
    wanted = {32, 33} if codec == "hevc" else {7}
    return wanted <= set(nal_types(data, codec))


def annex_b(extradata: bytes | None) -> bytes | None:
    """Codec extradata as Annex B, or None when it is not (or there is none)."""
    if extradata and (extradata.startswith(START) or extradata.startswith(b"\x00" + START)):
        return bytes(extradata)
    return None


class Kept:
    """What a camera sent since its last keyframe, and who is listening.

    Only touched from the event loop.
    """

    def __init__(self) -> None:
        self.frames: list[Frame] = []
        self.bytes = 0
        self.listeners: set[Listener] = set()

    def add(self, frame: Frame) -> None:
        if frame.keyframe:
            self.frames = [frame]
            self.bytes = len(frame.data)
        elif self.frames:
            self.frames.append(frame)
            self.bytes += len(frame.data)
            if self.bytes > MAX_KEPT_BYTES:
                self.clear()
        for listener in self.listeners:
            listener.offer(frame)

    def clear(self) -> None:
        self.frames = []
        self.bytes = 0


class Listener:
    """One TV's place in a relay: the frames it has yet to be sent."""

    def __init__(self, kept: Kept) -> None:
        self.queue: asyncio.Queue[bytes | None] = asyncio.Queue()
        # Without a keyframe to start from, frames are of no use until the next one.
        self.waiting = not kept.frames
        for frame in kept.frames:
            self.queue.put_nowait(frame.encode(cached=True))

    def offer(self, frame: Frame) -> None:
        if self.waiting and not frame.keyframe:
            return
        if self.queue.qsize() > MAX_QUEUED:
            # Too far behind to catch up: drop what is queued and resume at a keyframe.
            while not self.queue.empty():
                self.queue.get_nowait()
            self.waiting = not frame.keyframe
            if self.waiting:
                return
        self.waiting = False
        self.queue.put_nowait(frame.encode())

    def end(self) -> None:
        self.queue.put_nowait(None)


def read_source(source: str, options: dict[str, str]) -> Iterator[tuple[str, int, int] | Frame]:
    """Reads a camera's stream with PyAV: first (codec, width, height), then its frames.

    Runs on the relay's thread. Raises when the stream cannot be opened, is not H.264 or HEVC,
    or ends.
    """
    import av
    from av.bitstream import BitStreamFilterContext

    with av.open(source, options=options, timeout=(10, 10)) as container:
        if not container.streams.video:
            raise ValueError("the camera has no video")
        stream = container.streams.video[0]
        context = stream.codec_context
        codec = CODECS.get(context.name)
        if codec is None:
            raise ValueError(f"{context.name} video cannot be relayed")
        parameters = annex_b(context.extradata)
        # A stream from a file or HTTP holds length-prefixed packets: make them Annex B.
        to_annex_b = None
        if context.extradata and parameters is None:
            to_annex_b = BitStreamFilterContext(f"{codec}_mp4toannexb", stream)
        yield codec, context.width, context.height
        started = time.monotonic()
        for packet in container.demux(stream):
            if packet.size == 0:
                continue
            for out in to_annex_b.filter(packet) if to_annex_b else (packet,):
                data = bytes(out)
                keyframe = bool(out.is_keyframe)
                if keyframe and parameters and not has_parameter_sets(data, codec):
                    data = parameters + data
                stamp = out.pts if out.pts is not None else out.dts
                # A stream without timestamps is stamped as it arrives.
                pts = (
                    int(stamp * out.time_base * 1_000_000)
                    if stamp is not None and out.time_base
                    else int((time.monotonic() - started) * 1_000_000)
                )
                yield Frame(data, pts, keyframe)
    raise ValueError("the stream ended")


Reader = Callable[[str, dict[str, str]], Iterator[Any]]


class Relay:
    """One camera's stream, read once and passed on to every TV that listens."""

    def __init__(
        self, hass: HomeAssistant, name: str, source: str, options: dict[str, str], reader: Reader
    ) -> None:
        self.hass = hass
        self.name = name
        self.source = source
        self.options = options
        self.reader = reader
        self.kept = Kept()
        self.format: tuple[str, int, int] | None = None
        self.ready = asyncio.Event()
        self.error: str | None = None
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._linger: asyncio.TimerHandle | None = None

    @property
    def running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    @callback
    def start(self) -> None:
        """Starts reading (if it is not), and stops again unless someone listens in time."""
        # A reader told to stop may still be waiting on its camera: it is replaced, not reused.
        if not self.running or self._stop.is_set():
            self._stop = threading.Event()
            self.error = None
            self._thread = threading.Thread(
                target=self._run, args=(self._stop,), name=f"luma-relay-{self.name}", daemon=True
            )
            self._thread.start()
        self._rest()

    @callback
    def stop(self) -> None:
        self._stop.set()
        if self._linger:
            self._linger.cancel()
            self._linger = None
        for listener in self.kept.listeners:
            listener.end()
        self.kept.clear()
        self.ready.clear()
        self.format = None

    @callback
    def _rest(self) -> None:
        if self._linger:
            self._linger.cancel()
            self._linger = None
        if not self.kept.listeners:
            self._linger = self.hass.loop.call_later(LINGER, self.stop)

    @callback
    def listen(self) -> Listener:
        self.start()
        listener = Listener(self.kept)
        self.kept.listeners.add(listener)
        self._rest()
        return listener

    @callback
    def leave(self, listener: Listener) -> None:
        self.kept.listeners.discard(listener)
        self._rest()

    def _run(self, stop: threading.Event) -> None:
        """The relay's thread: reads the camera until stopped, reconnecting when it fails."""
        post = self.hass.loop.call_soon_threadsafe
        while not stop.is_set():
            try:
                for item in self.reader(self.source, self.options):
                    if stop.is_set():
                        return
                    if isinstance(item, Frame):
                        post(self._frame, stop, item)
                    else:
                        post(self._opened, stop, item)
            except Exception as err:
                post(self._failed, stop, str(err) or type(err).__name__)
            if stop.wait(RETRY):
                return

    @callback
    def _opened(self, stop: threading.Event, format: tuple[str, int, int]) -> None:
        if stop.is_set():
            return
        self.format = format
        self.error = None
        self.kept.clear()
        self.ready.set()

    @callback
    def _frame(self, stop: threading.Event, frame: Frame) -> None:
        if not stop.is_set():
            self.kept.add(frame)

    @callback
    def _failed(self, stop: threading.Event, error: str) -> None:
        if stop.is_set():
            return
        _LOGGER.debug("Relay %s: %s", self.name, error)
        self.error = error
        self.kept.clear()
        # Listeners end: their TV asks again, or shows the camera another way.
        for listener in self.kept.listeners:
            listener.end()
        self.ready.set()


class Relays:
    """Every running relay, by camera entity ID."""

    def __init__(self, hass: HomeAssistant, reader: Reader = read_source) -> None:
        self.hass = hass
        self.reader = reader
        self.relays: dict[str, Relay] = {}

    @callback
    def get(self, entity_id: str, source: str, options: dict[str, str]) -> Relay:
        """The camera's relay; a camera whose source changed gets a new one."""
        relay = self.relays.get(entity_id)
        if relay is not None and relay.source != source:
            relay.stop()
            relay = None
        if relay is None:
            relay = self.relays[entity_id] = Relay(
                self.hass, entity_id, source, options, self.reader
            )
        return relay

    @callback
    def stop(self) -> None:
        for relay in self.relays.values():
            relay.stop()
        self.relays.clear()
