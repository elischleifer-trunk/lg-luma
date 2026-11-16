"""Press remote keys on the TV from SSH, through a short-lived uinput keyboard. Runs on the TV:

    scp tools/tv-keys.py tv:/tmp/ && ssh tv 'python3 /tmp/tv-keys.py 28'

Arguments are Linux key codes: 28 Enter (OK), 1 Escape (Back), 103/108/105/106 Up/Down/Left/Right.
"""

import fcntl
import os
import struct
import sys
import time

UI_SET_EVBIT = 0x40045564
UI_SET_KEYBIT = 0x40045565
UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
EV_SYN = 0
EV_KEY = 1
BUS_USB = 3  # what the TV was tested with


def main() -> None:
    keys = [int(code) for code in sys.argv[1:]]
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
    for code in range(1, 256):
        fcntl.ioctl(fd, UI_SET_KEYBIT, code)
    # uinput_user_dev: name, input_id, ff_effects_max, then four absolute-axis arrays.
    name = b"luma-debug-keys".ljust(80, b"\0")
    os.write(fd, name + struct.pack("HHHHi", BUS_USB, 1, 1, 1, 0) + bytes(4 * 64 * 4))
    fcntl.ioctl(fd, UI_DEV_CREATE)
    time.sleep(1)  # let the TV's input stack pick up the new device

    def event(kind: int, code: int, value: int) -> None:
        os.write(fd, struct.pack("llHHi", 0, 0, kind, code, value))

    try:
        for code in keys:
            event(EV_KEY, code, 1)
            event(EV_SYN, 0, 0)
            time.sleep(0.08)
            event(EV_KEY, code, 0)
            event(EV_SYN, 0, 0)
            time.sleep(0.6)
        time.sleep(0.5)
    finally:
        fcntl.ioctl(fd, UI_DEV_DESTROY)
        os.close(fd)


if __name__ == "__main__":
    main()
