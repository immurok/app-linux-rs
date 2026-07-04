#!/usr/bin/env python3
"""BLE GATT helper — handles all BLE I/O via a single dbus-fast connection.

All GATT operations (CMD write, RSP notify, OTA read/write) go through
one D-Bus connection. This is required because BlueZ only delivers
PropertiesChanged signals reliably to the connection that owns the
notification subscription.

Protocol (stdin/stdout, line-based):
  → CMD_WRITE:<hex>      Write to CMD characteristic
  ← WRITE_OK
  ← WRITE_ERR:<msg>
  → OTA_WRITE:<hex>      Write to OTA characteristic
  ← WRITE_OK / WRITE_ERR
  → OTA_READ             Read OTA characteristic
  ← READ_OK:<hex> / READ_ERR
  ← NOTIFY:<hex>         Async RSP notification (sent anytime)
  ← DISCONNECT           Device disconnected
  ← READY                Initialization complete
"""

import asyncio
import sys

from dbus_fast import BusType
from dbus_fast.aio import MessageBus


async def start_notify_with_retry(iface, label, retries=5, delay=0.3):
    """Enable GATT notifications, tolerating transient ATT errors.

    BlueZ/the firmware can return ATT 0x0e ("Unlikely Error") when the CCCD
    write races a just-restarted daemon re-subscribing on a device that stayed
    connected (the HID-keyboard anchor keeps the LE link up across daemon
    restarts). Retrying with a short delay clears the transient case.

    Returns True on success, False if every attempt failed. Callers treat
    False as best-effort and CONTINUE rather than letting an unhandled
    exception crash the whole helper: a crash is read upstream as a device
    disconnect and triggers a reconnect loop.
    """
    for attempt in range(1, retries + 1):
        try:
            await iface.call_start_notify()
            return True
        except Exception as e:
            sys.stderr.write(f"StartNotify {label} attempt {attempt}/{retries}: {e}\n")
            sys.stderr.flush()
            if attempt < retries:
                await asyncio.sleep(delay)
    return False


async def main():
    if len(sys.argv) < 4:
        print("Usage: ble-notify-helper.py <device_path> <cmd_path> <rsp_path> [ota_path] [extra_notify...]", file=sys.stderr)
        sys.exit(1)

    device_path = sys.argv[1]
    cmd_path = sys.argv[2]
    rsp_path = sys.argv[3]
    # argv[4] is OTA path (empty string if unavailable), rest are extra notify paths.
    ota_path = None
    extra_paths = []
    if len(sys.argv) > 4:
        ota_path = sys.argv[4] if sys.argv[4] else None
        extra_paths = sys.argv[5:]

    bus = await MessageBus(bus_type=BusType.SYSTEM).connect()

    # CMD characteristic interface
    cmd_intro = await bus.introspect("org.bluez", cmd_path)
    cmd_obj = bus.get_proxy_object("org.bluez", cmd_path, cmd_intro)
    cmd_iface = cmd_obj.get_interface("org.bluez.GattCharacteristic1")

    # RSP characteristic — subscribe to notifications
    rsp_intro = await bus.introspect("org.bluez", rsp_path)
    rsp_obj = bus.get_proxy_object("org.bluez", rsp_path, rsp_intro)
    rsp_iface = rsp_obj.get_interface("org.bluez.GattCharacteristic1")
    rsp_props = rsp_obj.get_interface("org.freedesktop.DBus.Properties")

    def on_rsp_changed(iface, changed, inv):
        if "Value" in changed:
            val = changed["Value"]
            if hasattr(val, "value"):
                val = val.value
            data = bytes(val)
            if data:
                sys.stdout.write(f"NOTIFY:{data.hex()}\n")
                sys.stdout.flush()

    rsp_props.on_properties_changed(on_rsp_changed)
    # RSP notifications carry FP-match/auth events — important, so retry hard.
    # But NEVER let a transient ATT error (0x0e) propagate: an unhandled
    # exception here kills the helper, which the daemon reads as a disconnect
    # and turns into a reconnect loop. On exhaustion, continue best-effort —
    # the link is up and the CCCD is usually already armed from a prior session.
    if not await start_notify_with_retry(rsp_iface, "RSP"):
        sys.stderr.write(
            "RSP StartNotify failed after retries — continuing best-effort "
            "(link alive; notifications likely already enabled)\n"
        )
        sys.stderr.flush()

    # OTA characteristic interface (optional)
    ota_iface = None
    if ota_path:
        try:
            ota_intro = await bus.introspect("org.bluez", ota_path)
            ota_obj = bus.get_proxy_object("org.bluez", ota_path, ota_intro)
            ota_iface = ota_obj.get_interface("org.bluez.GattCharacteristic1")
        except Exception:
            pass

    # StartNotify on extra characteristics (HID etc.)
    for path in extra_paths:
        try:
            intro = await bus.introspect("org.bluez", path)
            obj = bus.get_proxy_object("org.bluez", path, intro)
            iface = obj.get_interface("org.bluez.GattCharacteristic1")
            await iface.call_start_notify()
        except Exception:
            pass

    # Monitor device disconnect
    dev_intro = await bus.introspect("org.bluez", device_path)
    dev_obj = bus.get_proxy_object("org.bluez", device_path, dev_intro)
    dev_props = dev_obj.get_interface("org.freedesktop.DBus.Properties")

    disconnected = False

    def on_dev_changed(iface, changed, inv):
        nonlocal disconnected
        if "Connected" in changed:
            val = changed["Connected"]
            if hasattr(val, "value"):
                val = val.value
            if not val:
                disconnected = True
                sys.stdout.write("DISCONNECT\n")
                sys.stdout.flush()

    dev_props.on_properties_changed(on_dev_changed)

    sys.stdout.write("READY\n")
    sys.stdout.flush()

    # Process commands from stdin
    loop = asyncio.get_event_loop()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    await loop.connect_read_pipe(lambda: protocol, sys.stdin)

    while not disconnected:
        try:
            line = await asyncio.wait_for(reader.readline(), timeout=1.0)
        except asyncio.TimeoutError:
            continue
        except Exception:
            break

        if not line:
            break

        cmd = line.decode().strip()
        if not cmd:
            continue

        try:
            if cmd.startswith("CMD_WRITE:"):
                data = bytes.fromhex(cmd[10:])
                await cmd_iface.call_write_value(data, {})
                sys.stdout.write("WRITE_OK\n")
                sys.stdout.flush()

            elif cmd.startswith("OTA_WRITE:"):
                if not ota_iface:
                    sys.stdout.write("WRITE_ERR:no_ota\n")
                    sys.stdout.flush()
                    continue
                data = bytes.fromhex(cmd[10:])
                await ota_iface.call_write_value(data, {})
                sys.stdout.write("WRITE_OK\n")
                sys.stdout.flush()

            elif cmd == "OTA_READ":
                if not ota_iface:
                    sys.stdout.write("READ_ERR:no_ota\n")
                    sys.stdout.flush()
                    continue
                val = await ota_iface.call_read_value({})
                sys.stdout.write(f"READ_OK:{bytes(val).hex()}\n")
                sys.stdout.flush()

            elif cmd == "QUIT":
                break

        except Exception as e:
            sys.stdout.write(f"WRITE_ERR:{e}\n")
            sys.stdout.flush()

    bus.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
