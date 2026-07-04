#!/usr/bin/env python3
"""Hermetic test for ble-notify-helper.py's StartNotify retry logic.

Reproduces the bug where a single transient ATT error (e.g. 0x0e seen when a
just-restarted daemon re-subscribes on a still-connected device) crashed the
whole helper — read upstream as a disconnect, triggering a reconnect loop.

`dbus_fast` is stubbed in sys.modules so this runs under any python3 (the
daemon itself uses the system python that ships dbus_fast). No device needed.
"""

import asyncio
import importlib.util
import os
import sys
import types

# Stub dbus_fast so the helper's top-level imports succeed without the real dep.
_fake = types.ModuleType("dbus_fast")
_fake.BusType = object
sys.modules["dbus_fast"] = _fake
_fake_aio = types.ModuleType("dbus_fast.aio")
_fake_aio.MessageBus = object
sys.modules["dbus_fast.aio"] = _fake_aio

_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ble-notify-helper.py")
_spec = importlib.util.spec_from_file_location("ble_notify_helper", _path)
helper = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(helper)


class FakeIface:
    """Fake GATT characteristic: fails call_start_notify the first `fail_times`."""

    def __init__(self, fail_times):
        self.calls = 0
        self.fail_times = fail_times

    async def call_start_notify(self):
        self.calls += 1
        if self.calls <= self.fail_times:
            raise Exception(f"Operation failed with ATT error: 0x0e (simulated #{self.calls})")


fails = 0


def check(cond, name):
    global fails
    if cond:
        print(f"PASS: {name}")
    else:
        print(f"FAIL: {name}")
        fails += 1


async def run():
    # 1. Succeeds first try → True, called once.
    f = FakeIface(0)
    ok = await helper.start_notify_with_retry(f, "RSP", retries=5, delay=0)
    check(ok is True, "首次即成功 → True")
    check(f.calls == 1, "首次即成功 → 只调用一次")

    # 2. Transient: fails twice then succeeds → True, called 3 times.
    f = FakeIface(2)
    ok = await helper.start_notify_with_retry(f, "RSP", retries=5, delay=0)
    check(ok is True, "两次瞬时失败后成功 → True")
    check(f.calls == 3, "两次瞬时失败后成功 → 调用 3 次")

    # 3. Persistent failure → False (caller continues best-effort, no crash),
    #    attempted exactly `retries` times.
    f = FakeIface(99)
    ok = await helper.start_notify_with_retry(f, "RSP", retries=3, delay=0)
    check(ok is False, "持续失败 → False（不抛异常打死 helper）")
    check(f.calls == 3, "持续失败 → 恰好重试 retries 次")


asyncio.run(run())
sys.exit(1 if fails else 0)
