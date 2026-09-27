# dramma 

Donation kiosk + arcade machine for Hacker Embassy.  
Accepts cash (bills + coins), processes donations, and now lets people insert coins to play retro games on RetroArch.

---

## Running

```bash
nix-shell
cargo run
```

The app starts fullscreen. Tap the logo **5 times** to open the diagnostics panel (password-protected if `diagnostics_password` is set — see below).

---

## Configuration

Create `.config/dramma.toml` next to the binary (or in the working directory you run from):

```toml
token = "your-bearer-token" # For Bot donates
diagnostics_password = "your-password" # Optional — gates the diagnostics panel (and donation wall) if set

home_assistant_token = "your-long-lived-access-token" # Optional — enables the sensor.dramma_health push described below

# Optional overrides (these are the defaults):
home_assistant_url    = "https://ha.hackem.cc/web-dramma/0?BrowserID=dramma"
home_assistant_api_url = "https://ha.hackem.cc"
cashcode_serial_port  = "/dev/serial/by-id/usb-Prolific_Technology_Inc._USB-Serial_Controller_D-if00-port0"
cctalk_serial_port    = "/dev/ttyUSB0"
stats_db_path         = "data/Stats.db"
acceptor_restart_timeout_secs = 300 # 0 disables automatic computer reboots
```

The CashCode bill acceptor automatically closes and reopens its serial port after
an I/O failure, retrying every 5 seconds and resetting the device before resuming.
It restores the latest enable/disable request, including requests made while
disconnected. **Reset Bill Acceptor** in diagnostics also reopens the connection
and leaves bill acceptance disabled until the UI requests it again.

## Acceptor health and automatic recovery

While either CashCode or ccTalk is initializing or unavailable, the kiosk shows
"We've encountered a technical issue" with `ui/assets/under_construction.png`.
Payments stop, and the payment page and inserted amount are retained in memory
until both acceptors recover. Inactivity timers pause during the outage.
Tap the construction image **5 times** to access the usual diagnostics panel.
**This skips the `diagnostics_password` gate** (unlike the same gesture on the
main page) — temporary, until the virtual keyboard's password-reentry bug is
fixed; anyone at the machine can reach diagnostics while it's showing this
screen. Home Assistant's browser is closed while unavailable; an active game
is ended so the issue page is visible.

The drivers keep reconnecting. If either acceptor remains unavailable for
`acceptor_restart_timeout_secs` (5 minutes by default), the watchdog requests a
Linux computer reboot. A successful poll after initialization clears that
acceptor's outage timer; reconnect attempts do not reset it. A worker with no
successful poll for 30 seconds is also considered unavailable. A failed reboot
command is reported in the health response and retried once per minute.

If 3 consecutive reboots in a row don't lead to a single successful poll,
automatic rebooting stops — the fault is presumably hardware, not something a
restart fixes, and looping forever just means longer, more frequent outages.
The health response reports this ("Automatic reboot disabled... manual
intervention required") instead of the usual acceptor error, so the
Home Assistant sensor and its automations still surface it. This streak is
tracked in `data/reboot_watchdog_state` (survives reboots since the process's
own memory doesn't) and resets the moment either acceptor has a successful
poll — delete that file to reset it manually.

Both deployment methods install a sudoers rule allowing the kiosk user only
`/usr/bin/systemctl --no-block reboot`. For an existing installation, run the
permission installer as root before starting the updated binary:

```bash
ssh root@dramma.lan 'sh -s -- dramma' < scripts/install-reboot-permission.sh
```

When `home_assistant_token` is set, acceptor health is pushed into Home Assistant
as `sensor.dramma_health` (created automatically on first push) whenever it
changes — `state` is `ok` or `error`, with `errors`, `restart_in_secs` and
`restart_requested` as attributes. Recovery pushes immediately, but an error
only pushes if it's still unhealthy 15 seconds later — every startup briefly
reports both acceptors as "Initializing" before their first poll, which isn't
a real fault and would otherwise page someone on every deploy or reboot. The
kiosk's own screen isn't affected by this delay; it still reacts instantly.
This monitors the acceptors and reboot watchdog; it does not probe the
donation backend or receipt printer. Because
it's push-based, a fully hung or crashed process can't report its own failure —
pair it with a Home Assistant automation on the entity's `last_updated` (or
`availability`/timeout template) if you need to detect total silence, not just
acceptor errors.

Example attributes on failure:

```json
{
  "friendly_name": "Dramma Health",
  "errors": [
    {"component": "cashcode", "message": "serial port error: No such file or directory", "unavailable_for_secs": 42}
  ],
  "restart_in_secs": 258,
  "restart_requested": false
}
```

---

## 🕹️ Setting Up Games (Arcade Mode)

Pressing **PLAY** on the main screen takes the user to the coin-insertion screen where they can select a game and insert money:

> **100 AMD = 5 minutes of playtime**  
> **50 AMD = 2 minutes 30 seconds**

When they hit **Launch**, dramma starts RetroArch fullscreen + kiosk and auto-closes it when the time runs out. The speaker will announce "2 minutes left" and "1 minute left".

> ROMs are not included for obvious copyright reasons. You know where to find them.

### Configure games in dramma.toml

Add a `[[games]]` block for each game. `name` is what shows up in the UI, `core` is the path to the `.so`, `rom` is the path to your ROM file.

```toml
retroarch_command = "retroarch"

[[games]]
name = "🧱 Tetris"
core = "/etc/retroarch/cores/nestopia_libretro.so"
rom  = "/home/dramma/roms/tetris.nes"

[[games]]
name = "🟡 Pac-Man"
core = "/etc/retroarch/cores/fbneo_libretro.so"
rom  = "/home/dramma/roms/pacman.zip"

[[games]]
name = "🦔 Sonic the Hedgehog"
core = "/etc/retroarch/cores/picodrive_libretro.so"
rom  = "/home/dramma/roms/sonic.md"

[[games]]
name = "🔫 DOOM"
core = "/etc/retroarch/cores/prboom_libretro.so"
rom  = "/home/dramma/roms/doom.wad"

[[games]]
name = "👊 Street Fighter II"
core = "/etc/retroarch/cores/fbneo_libretro.so"
rom  = "/home/dramma/roms/sf2.zip"
```

If `[[games]]` is **not configured**, the UI shows a built-in placeholder list (same names, no actual cores/ROMs). RetroArch will still launch but will open its own menu — not useful in production.

`rom` must be an actual IWAD (`file` should report "doom main IWAD data", not "doom patch PWAD data") — the shareware `doom1.wad` works fine. The PrBoom core also needs its own `prboom.wad` (a separate engine-resource file, unrelated to the game WAD despite the confusingly similar name) in RetroArch's `system_directory` — without it the core won't load at all.

### Test it manually first

Before trusting the machine to do it, test the exact command dramma will run:

```bash
retroarch --fullscreen --libretro /path/to/core_libretro.so /path/to/rom
```

Run RetroArch as the logged-in kiosk user. Launching it through `sudo` strips the
desktop audio session environment and can leave a black fullscreen window.

If the game boots correctly, the config is right. If RetroArch opens its menu instead of loading the game, the core or ROM path is wrong.


---

## Money → Time conversion

| Inserted | Play time |
|---|---|
| 50 ֏ | 2 min 30 sec |
| 100 ֏ | 5 min |
| 200 ֏ | 10 min |
| 500 ֏ | 25 min |

---

## Architecture

```
main.rs
├── bill_acceptor      — CashCode bill acceptor driver (serial)
├── coin_acceptor      — ccTalk coin acceptor driver (serial)
├── donation_handler   — Donation flow + inactivity timeout
├── game_handler       — Arcade mode: RetroArch lifecycle + session timer
├── home_assistant_handler — Chromium kiosk for HASS page
└── diagnostics_handler — Debug log viewer

src/
├── cashcode.rs        — CashCode serial protocol
├── cashcode_driver.rs — CashCode reconnect and command handling
├── cctalk.rs          — ccTalk serial protocol
├── config.rs          — dramma.toml loader
├── retroarch.rs       — RetroArch process manager
├── sound.rs           — Audio (yippee + time warnings)
└── ...

ui/
├── pages/
│   ├── main.slint          — Main screen (Donate / Play / HASS)
│   ├── insert_coins.slint  — Game selector + coin insertion
│   ├── insert_money.slint  — Donation coin insertion
│   ├── donate.slint        — Donation form
│   └── ...
└── assets/
    ├── yippee.wav
    ├── two_minutes_left.wav
    └── one_minute_left.wav
```
