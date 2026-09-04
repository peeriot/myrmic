#!/usr/bin/env python3
"""BLE load-generator peripheral for watchdog timeout characterization (#1014).

Advertises a GATT server with a deliberately large service/characteristic table
plus a few notifying characteristics. The ESP32 firmware (the `wdt-loadgen`
cell) connects to it as central, discovers the whole table, and subscribes —
driving the "active connection + heavy GATT discovery" worst case the watchdog
timeouts must survive (SDS-FEAT-2026-HWD-001 constraint 6).

Why a fat table: the firmware builds the discovery blob on the guest's WASM
stack, so a bigger table squeezes the host heap available for the connection —
the exact memory-pressure worst case. The firmware caps at
MAX_SUPPORTED_SERVICES=16 x MAX_SUPPORTED_CHARACTERISTICS=8, so this defaults
near that ceiling. Dial N_SERVICES / N_CHARS down for a lighter run.

Requires: `pip install bless` (a BlueZ-backed GATT server on Linux).
Run: `python3 wdt_loadgen_peripheral.py`  (needs BlueZ; may need sudo/caps).

The advertised name WDT-LOADGEN is what the cell's DiscoveryFilter matches.
"""

import asyncio
import struct
import logging

from bless import (  # type: ignore
    BlessServer,
    BlessGATTCharacteristic,
    GATTCharacteristicProperties,
    GATTAttributePermissions,
)

logging.basicConfig(level=logging.INFO)
log = logging.getLogger("wdt-loadgen")

DEVICE_NAME = "WDT-LOADGEN"

# Table size. Keep at/under the firmware caps (16 services x 8 characteristics).
N_SERVICES = 8
N_CHARS = 8

# How many characteristics (spread across services) push notifications, and how
# often — this is the sustained radio traffic during the held connection.
N_NOTIFIERS = 4
NOTIFY_PERIOD_S = 0.2

# Base 128-bit UUID; bytes 6..8 (little dashes) carry the service/char indices so
# every UUID is distinct and reconstructable.
def svc_uuid(s: int) -> str:
    return f"5744_4700-{s:04x}-0000-0000-0000_00000000".replace("_", "")


def char_uuid(s: int, c: int) -> str:
    return f"5744_4701-{s:04x}-{c:04x}-0000-0000_00000000".replace("_", "")


def notifiers() -> list[tuple[str, str]]:
    # First characteristic of the first N_NOTIFIERS services notifies. Track the
    # (service, characteristic) pair so update_value gets the right service.
    return [(svc_uuid(s), char_uuid(s, 0)) for s in range(min(N_NOTIFIERS, N_SERVICES))]


async def build_table(server: BlessServer) -> None:
    notify_chars = {cu for _su, cu in notifiers()}
    for s in range(N_SERVICES):
        await server.add_new_service(svc_uuid(s))
        for c in range(N_CHARS):
            cu = char_uuid(s, c)
            props = (
                GATTCharacteristicProperties.read
                | GATTCharacteristicProperties.write
            )
            if cu in notify_chars:
                props |= GATTCharacteristicProperties.notify
            await server.add_new_characteristic(
                svc_uuid(s),
                cu,
                props,
                bytearray([s & 0xFF, c & 0xFF]),
                GATTAttributePermissions.readable
                | GATTAttributePermissions.writeable,
            )
    log.info(
        "GATT table: %d services x %d chars (%d total), %d notifiers",
        N_SERVICES,
        N_CHARS,
        N_SERVICES * N_CHARS,
        len(notifiers()),
    )


def _read_request(char: BlessGATTCharacteristic, **_) -> bytearray:
    return char.value


def _write_request(char: BlessGATTCharacteristic, value: bytearray, **_) -> None:
    char.value = value


async def main() -> None:
    server = BlessServer(name=DEVICE_NAME)
    server.read_request_func = _read_request
    server.write_request_func = _write_request

    await build_table(server)
    await server.start()
    log.info("advertising as %r — waiting for the ESP32 to connect", DEVICE_NAME)

    counter = 0
    try:
        while True:
            await asyncio.sleep(NOTIFY_PERIOD_S)
            counter = (counter + 1) & 0xFFFFFFFF
            payload = struct.pack("<I", counter)
            for su, cu in notifiers():
                char = server.get_characteristic(cu)
                if char is not None:
                    char.value = bytearray(payload)
                    server.update_value(su, cu)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        await server.stop()
        log.info("stopped")


if __name__ == "__main__":
    asyncio.run(main())
