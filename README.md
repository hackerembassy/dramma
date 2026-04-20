# dramma 

Donation kiosk + arcade machine for Hacker Embassy.  
Accepts cash (bills + coins), processes donations, and now lets people insert coins to play retro games on RetroArch.

---

## Running

```bash
nix-shell
cargo run
```

The app starts fullscreen. Tap the logo **5 times** to open the diagnostics panel.

---

## Configuration

Create `.config/dramma.toml` next to the binary (or in the working directory you run from):

```toml
token = "your-bearer-token" # For Bot donates

# Optional overrides (these are the defaults):
home_assistant_url    = "https://ha.hackem.cc/web-dramma/0?BrowserID=dramma"
cashcode_serial_port  = "/dev/serial/by-id/usb-Prolific_Technology_Inc._USB-Serial_Controller_D-if00-port0"
cctalk_serial_port    = "/dev/ttyUSB0"
stats_db_path         = "data/Stats.db"
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
core = "/etc/retroarch/cores/dosbox_pure_libretro.so"
rom  = "/home/dramma/roms/doom.wad"

[[games]]
name = "👊 Street Fighter II"
core = "/etc/retroarch/cores/fbneo_libretro.so"
rom  = "/home/dramma/roms/sf2.zip"
```

If `[[games]]` is **not configured**, the UI shows a built-in placeholder list (same names, no actual cores/ROMs). RetroArch will still launch but will open its own menu — not useful in production.

### Test it manually first

Before trusting the machine to do it, test the exact command dramma will run:

```bash
retroarch --fullscreen --kiosk -L /path/to/core_libretro.so /path/to/rom
```

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
