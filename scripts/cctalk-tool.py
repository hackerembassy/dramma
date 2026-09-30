#!/usr/bin/env python3
"""Maintenance tool for the ccTalk coin acceptor: inspect, test, relabel, teach.

Speaks ccTalk directly (9600 8N1, simple 8-bit checksum, host address 1,
coin acceptor address 2) and consumes the local echo like src/cctalk.rs does.
Unlike dramma's transport it treats NAK / BUSY replies as errors, so an
unsupported command is reported instead of looking like a success.

Needs pyserial. Stop dramma first -- the serial port is opened exclusively.
See "Coin acceptor maintenance" in README.md for teaching new coins.
"""

import argparse
import signal
import subprocess
import sys
import termios
import time

import serial

# macOS names the CP2102 adapter after its serial number; on the kiosk use the
# same by-id path as cctalk_serial_port in dramma.toml.
DEFAULT_PORT = (
    "/dev/cu.usbserial-0001"
    if sys.platform == "darwin"
    else "/dev/serial/by-id/usb-Silicon_Labs_CP2102_USB_to_UART_Bridge_Controller_0001-if00-port0"
)

HOST_ADDR = 1
ACK, NAK, BUSY = 0, 5, 6

H_RESET = 1
H_COMMS_REVISION = 4
H_REQUEST_COIN_ID = 184
H_MODIFY_COIN_ID = 185
H_BUILD_CODE = 192
H_CONFIG_TO_EEPROM = 199
H_TEACH_STATUS = 201
H_TEACH_MODE = 202
H_OPTION_FLAGS = 213
H_REQUEST_MASTER_INHIBIT = 227
H_MODIFY_MASTER_INHIBIT = 228
H_READ_CREDITS = 229
H_REQUEST_INHIBITS = 230
H_MODIFY_INHIBITS = 231
H_TEST_SOLENOIDS = 240
H_SOFTWARE_REVISION = 241
H_SERIAL_NUMBER = 242
H_DATABASE_VERSION = 243
H_PRODUCT_CODE = 244
H_CATEGORY = 245
H_MANUFACTURER = 246
H_POLLING_PRIORITY = 249
H_SIMPLE_POLL = 254

TEACH_STATUS = {252: "aborted", 253: "error", 254: "in progress", 255: "completed"}

COIN_ERRORS = {
    0: "null event",
    1: "reject coin",
    2: "inhibited coin",
    3: "multiple window",
    4: "wake-up timeout",
    5: "validation timeout",
    6: "credit sensor timeout",
    7: "sorter opto timeout",
    8: "2nd close coin error",
    9: "accept gate not ready",
    10: "credit sensor not ready",
    11: "sorter not ready",
    12: "reject coin not cleared",
    13: "validation sensor not ready",
    14: "credit sensor blocked",
    15: "sorter opto blocked",
    16: "credit sequence error",
    17: "coin going backwards",
    18: "coin too fast (credit sensor)",
    19: "coin too slow (credit sensor)",
    20: "coin-on-string mechanism activated",
    21: "DCE opto timeout",
    22: "DCE opto not seen",
    23: "credit sensor reached too early",
    24: "reject coin (repeated sequential trip)",
    25: "reject slug",
    26: "reject sensor blocked",
    27: "games overload",
    28: "max coin meter pulses exceeded",
    29: "accept gate open not closed",
    30: "accept gate closed not open",
    31: "manifold opto timeout",
    32: "manifold opto blocked",
    33: "manifold not ready",
    34: "security status changed",
    35: "motor exception",
    36: "swallowed coin",
    37: "coin too fast (validation sensor)",
    38: "coin too slow (validation sensor)",
    39: "coin incorrectly sorted",
    40: "external light attack",
    254: "coin return mechanism activated",
    255: "unspecified alarm",
}


class CcTalkError(Exception):
    pass


class Nak(CcTalkError):
    pass


class Busy(CcTalkError):
    pass


class PortLost(CcTalkError):
    """The USB serial adapter went away (e.g. re-enumerated after a glitch)."""


class Device:
    def __init__(self, port, addr, echo, verbose):
        self.port = port
        self.addr = addr
        self.echo = echo
        self.verbose = verbose
        self.ser = self._open()

    def _open(self):
        ser = serial.Serial(
            self.port,
            9600,
            bytesize=serial.EIGHTBITS,
            parity=serial.PARITY_NONE,
            stopbits=serial.STOPBITS_ONE,
            timeout=0.02,
            exclusive=True,
        )
        time.sleep(0.1)
        return ser

    def close(self):
        self.ser.close()

    def reopen(self, timeout=30):
        """Waits for the port to come back and the device to answer a poll."""
        try:
            self.ser.close()
        except (OSError, termios.error):
            pass
        deadline = time.monotonic() + timeout
        while True:
            try:
                self.ser = self._open()
                self.cmd(H_SIMPLE_POLL)
                return
            except (OSError, termios.error, CcTalkError) as e:
                # Release the exclusive lock, or the next attempt can't reopen.
                try:
                    self.ser.close()
                except (OSError, termios.error):
                    pass
                if time.monotonic() > deadline:
                    raise PortLost(f"could not reconnect to {self.port}: {e}") from e
                time.sleep(0.5)

    def cmd(self, header, data=b"", timeout=1.0, retries=2):
        data = bytes(data)
        pkt = bytes([self.addr, len(data), HOST_ADDR, header]) + data
        pkt += bytes([(-sum(pkt)) & 0xFF])
        err = None
        for _ in range(retries + 1):
            try:
                return self._exchange(pkt, timeout)
            except (OSError, termios.error) as e:
                raise PortLost(str(e)) from e
            except (Nak, Busy):
                raise
            except CcTalkError as e:
                err = e
                time.sleep(0.1)
        raise err

    def _exchange(self, pkt, timeout):
        self.ser.reset_input_buffer()
        self.ser.write(pkt)
        self.ser.flush()
        deadline = time.monotonic() + timeout
        buf = bytearray()

        def need(n):
            while len(buf) < n and time.monotonic() < deadline:
                buf.extend(self.ser.read(n - len(buf)))
            return len(buf) >= n

        off = 0
        if self.echo:
            if not need(len(pkt)):
                raise CcTalkError(f"timeout waiting for echo (got {bytes(buf).hex(' ')})")
            if bytes(buf[: len(pkt)]) != pkt:
                raise CcTalkError(f"echo mismatch: {bytes(buf).hex(' ')}")
            off = len(pkt)
        if not need(off + 5):
            raise CcTalkError(f"timeout waiting for reply (got {bytes(buf[off:]).hex(' ')})")
        n = buf[off + 1]
        if not need(off + 5 + n):
            raise CcTalkError(f"short reply: {bytes(buf[off:]).hex(' ')}")
        reply = bytes(buf[off : off + 5 + n])
        if self.verbose:
            print(f"  -> {pkt.hex(' ')}\n  <- {reply.hex(' ')}", file=sys.stderr)
        if sum(reply) & 0xFF:
            raise CcTalkError(f"checksum error: {reply.hex(' ')}")
        if reply[0] != HOST_ADDR or reply[2] != self.addr:
            raise CcTalkError(f"unexpected addressing: {reply.hex(' ')}")
        if reply[3] == NAK:
            raise Nak(f"NAK for header {pkt[3]}")
        if reply[3] == BUSY:
            raise Busy(f"BUSY for header {pkt[3]}")
        if reply[3] != ACK:
            raise CcTalkError(f"unexpected reply header {reply[3]}: {reply.hex(' ')}")
        return reply[4 : 4 + n]

    def ascii(self, header, data=b""):
        return self.cmd(header, data).decode("ascii", errors="replace")

    def coin_id(self, pos):
        return self.ascii(H_REQUEST_COIN_ID, [pos])

    def inhibits(self):
        lo, hi = self.cmd(H_REQUEST_INHIBITS)
        mask = lo | (hi << 8)
        return [bool(mask & (1 << (pos - 1))) for pos in range(1, 17)]

    def set_inhibits(self, enabled):
        mask = sum(1 << (pos - 1) for pos, on in zip(range(1, 17), enabled) if on)
        self.cmd(H_MODIFY_INHIBITS, [mask & 0xFF, mask >> 8])

    def set_accepting(self, accepting):
        self.cmd(H_MODIFY_MASTER_INHIBIT, [1 if accepting else 0])


def amd_value(coin_id):
    """Mirrors parse_coin_id_amd() in src/cctalk.rs (value field in luma)."""
    if len(coin_id) < 5 or coin_id[:2] == "..":
        return None
    value = coin_id[2:5]
    if value == "000":
        return None
    try:
        if "K" in value:
            k = value.index("K")
            luma = int(value[:k]) * 1000 + int(value[k + 1 :] or 0) * 100
        else:
            luma = int(value)
    except ValueError:
        return None
    return luma // 100


def show(label, fn):
    try:
        value = fn()
    except CcTalkError as e:
        value = f"n/a ({e})"
    print(f"{label:<22} {value}")


def cmd_info(dev, _args):
    dev.cmd(H_SIMPLE_POLL)
    show("Manufacturer", lambda: dev.ascii(H_MANUFACTURER))
    show("Category", lambda: dev.ascii(H_CATEGORY))
    show("Product code", lambda: dev.ascii(H_PRODUCT_CODE))
    show("Build code", lambda: dev.ascii(H_BUILD_CODE))
    show("Software revision", lambda: dev.ascii(H_SOFTWARE_REVISION))
    show("Serial number", lambda: int.from_bytes(dev.cmd(H_SERIAL_NUMBER), "little"))
    show("Database version", lambda: dev.cmd(H_DATABASE_VERSION).hex(" "))
    show("Comms revision", lambda: ".".join(str(b) for b in dev.cmd(H_COMMS_REVISION)))
    show("Option flags", lambda: dev.cmd(H_OPTION_FLAGS).hex(" "))
    show("Polling priority", lambda: dev.cmd(H_POLLING_PRIORITY).hex(" "))
    show(
        "Master inhibit",
        lambda: "accepting" if dev.cmd(H_REQUEST_MASTER_INHIBIT)[0] & 1 else "inhibited",
    )
    show("Teach status", lambda: dev.cmd(H_TEACH_STATUS, [0]).hex(" "))

    try:
        enabled = dev.inhibits()
    except CcTalkError as e:
        print(f"Inhibit status         n/a ({e})")
        enabled = [None] * 16

    print("\nPos  Coin ID  Value     Enabled")
    for pos in range(1, 17):
        try:
            cid = dev.coin_id(pos)
        except CcTalkError as e:
            print(f"{pos:>3}  n/a ({e})")
            continue
        value = amd_value(cid)
        value_str = f"{value} AMD" if value is not None else "-"
        on = {True: "yes", False: "no", None: "?"}[enabled[pos - 1]]
        print(f"{pos:>3}  {cid!r:<8} {value_str:<9} {on}")


_speech = None


def say(args, text):
    """Spoken feedback (macOS `say`) for whoever is feeding coins at the device."""
    global _speech
    if not args.say:
        return
    if _speech and _speech.poll() is None:
        _speech.terminate()
    try:
        _speech = subprocess.Popen(["say", text], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except OSError:
        args.say = False  # no `say` outside macOS


def teach_status(dev):
    count, status = dev.cmd(H_TEACH_STATUS, [0], retries=0)
    return count, status, TEACH_STATUS.get(status, f"unknown ({status})")


def cmd_teach(dev, args):
    """Starts teach mode and reports the first status replies, leaving the
    device in teach mode for `teach-wait`."""
    pos = args.position
    print(f"Position {pos} before teach: {dev.coin_id(pos)!r}", flush=True)
    data = [pos] if args.orientation is None else [pos, args.orientation]
    dev.cmd(H_TEACH_MODE, data, timeout=3.0, retries=0)
    print(f"Teach mode control ACKed for position {pos}", flush=True)
    end = time.monotonic() + 2
    while time.monotonic() < end:
        try:
            count, _, label = teach_status(dev)
            print(f"Teach status: coins: {count}  status: {label}", flush=True)
            say(args, "Teach mode started")
            return 0
        except CcTalkError as e:
            err = e
            time.sleep(0.25)
    print(f"Teach status not answered: {err}", flush=True)
    return 3


def cmd_teach_wait(dev, args):
    start = time.monotonic()
    last = None
    last_reply = start
    try:
        while True:
            now = time.monotonic()
            try:
                count, status, label = teach_status(dev)
                last_reply = now
            except CcTalkError:
                if now - last_reply > 15:
                    print(f"Teach status not answered for {now - last_reply:.0f}s", flush=True)
                    say(args, "Device stopped answering")
                    return 3
                time.sleep(0.25)
                continue
            if (count, status) != last:
                print(f"[{now - start:6.1f}s] coins: {count:>3}  status: {label}", flush=True)
                if status == 254 and (last is None or count > last[0]) and count:
                    say(args, str(count))
                last = (count, status)
            if status == 255:
                say(args, "Teach complete")
                return 0
            if status in (252, 253):
                say(args, f"Teach {label}")
                return 1
            if now - start > args.timeout:
                print("Timed out, aborting teach", flush=True)
                break
            time.sleep(0.25)
    except KeyboardInterrupt:
        print("Interrupted, aborting teach", flush=True)
    return cmd_teach_abort(dev, args) or 2


def cmd_teach_abort(dev, args):
    try:
        count, status = dev.cmd(H_TEACH_STATUS, [1], retries=0)
        print(f"Abort reply: coins: {count}  status: {TEACH_STATUS.get(status, status)}", flush=True)
    except CcTalkError as e:
        print(f"Abort not answered ({e}), resetting device", flush=True)
        dev.cmd(H_RESET)
    say(args, "Teach aborted")
    return 0


def cmd_set_id(dev, args):
    cid = args.coin_id
    if len(cid) != 6 or not cid.isascii():
        sys.exit("coin ID must be exactly 6 ASCII characters, e.g. AM1K0A")
    before = dev.coin_id(args.position)
    dev.cmd(H_MODIFY_COIN_ID, [args.position, *cid.encode("ascii")], timeout=2.0)
    after = dev.coin_id(args.position)
    print(f"Position {args.position}: {before!r} -> {after!r} ({amd_value(after)} AMD)")
    return 0 if after == cid else 1


def cmd_eeprom(dev, _args):
    dev.cmd(H_CONFIG_TO_EEPROM, timeout=3.0)
    print("Configuration saved to EEPROM")
    return 0


def cmd_watch(dev, args):
    ids = {}
    for pos in range(1, 17):
        try:
            ids[pos] = dev.coin_id(pos)
        except CcTalkError:
            ids[pos] = "??????"
    def arm():
        # A device reset clears all coin enables, so this runs after every reconnect too.
        baseline = dev.cmd(H_READ_CREDITS)[0]
        dev.set_inhibits([True] * 16)
        dev.set_accepting(True)
        return baseline

    counter = arm()
    print(f"Accepting coins for {args.seconds}s -- insert coins now", flush=True)
    end = time.monotonic() + args.seconds
    try:
        while time.monotonic() < end:
            try:
                data = dev.cmd(H_READ_CREDITS)
            except PortLost as e:
                print(f"Serial port lost ({e}), reconnecting...", flush=True)
                say(args, "Connection lost")
                dev.reopen()
                counter = arm()
                print("Reconnected, accepting coins again", flush=True)
                say(args, "Reconnected")
                continue
            except CcTalkError as e:
                print(f"Poll failed: {e}", flush=True)
                time.sleep(0.5)
                continue
            current = data[0]
            if current != counter:
                if current == 0:
                    print("Device reset detected, re-enabling coins", flush=True)
                    counter = arm()
                    continue
                if counter == 0:
                    new = current
                elif current > counter:
                    new = current - counter
                else:
                    new = current - counter + 255
                if new > 5:
                    print(f"Lost {new - 5} events", flush=True)
                for i in reversed(range(min(new, 5))):
                    a, b = data[1 + 2 * i], data[2 + 2 * i]
                    if a:
                        cid = ids.get(a, "??????")
                        value = amd_value(cid)
                        print(f"CREDIT pos={a} id={cid!r} value={value} AMD path={b}", flush=True)
                        say(args, f"{value} dram" if value else f"position {a}")
                    elif b:
                        print(f"ERROR  code={b} ({COIN_ERRORS.get(b, 'unknown')})", flush=True)
                        say(args, COIN_ERRORS.get(b, "error"))
                counter = current
            time.sleep(0.1)
    except KeyboardInterrupt:
        pass
    finally:
        try:
            dev.set_accepting(False)
            print("Master inhibit restored (not accepting)", flush=True)
        except CcTalkError as e:
            print(f"Could not restore master inhibit: {e}", flush=True)
    return 0


def _interrupt(_signum, _frame):
    # Lets TaskStop / kill run the same clean-up (teach abort, re-inhibit) as Ctrl-C.
    raise KeyboardInterrupt


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--port", default=DEFAULT_PORT)
    p.add_argument("--addr", type=int, default=2, help="device address (coin acceptor default: 2)")
    p.add_argument("--no-echo", action="store_true", help="adapter does not echo transmitted bytes")
    p.add_argument("-v", "--verbose", action="store_true", help="dump raw packets")
    p.add_argument("--say", action="store_true", help="speak progress (macOS `say`)")
    sub = p.add_subparsers(dest="command", required=True)

    sub.add_parser("info", help="identify the device and list coin positions")

    t = sub.add_parser(
        "teach",
        help="start ccTalk teach mode (disabled on the kiosk's NRI G-13; use its S2 switches)",
    )
    t.add_argument("position", type=int, choices=range(1, 17), metavar="POSITION")
    t.add_argument("--orientation", type=int)

    tw = sub.add_parser("teach-wait", help="poll teach status until done (aborts on timeout)")
    tw.add_argument("--timeout", type=float, default=600)

    sub.add_parser("teach-abort", help="abort teach mode")

    s = sub.add_parser("set-id", help="relabel a coin position (Modify coin id)")
    s.add_argument("position", type=int, choices=range(1, 17), metavar="POSITION")
    s.add_argument("coin_id")

    sub.add_parser("eeprom", help="store configuration to EEPROM")

    w = sub.add_parser("watch", help="accept coins and print credits/errors")
    w.add_argument("--seconds", type=float, default=120)

    args = p.parse_args()
    signal.signal(signal.SIGTERM, _interrupt)
    dev = Device(args.port, args.addr, not args.no_echo, args.verbose)
    try:
        handler = {
            "info": cmd_info,
            "teach": cmd_teach,
            "teach-wait": cmd_teach_wait,
            "teach-abort": cmd_teach_abort,
            "set-id": cmd_set_id,
            "eeprom": cmd_eeprom,
            "watch": cmd_watch,
        }[args.command]
        sys.exit(handler(dev, args) or 0)
    except CcTalkError as e:
        sys.exit(f"ccTalk error: {e}")
    finally:
        dev.close()


if __name__ == "__main__":
    main()
