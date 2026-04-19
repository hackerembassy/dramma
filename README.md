# dramma 🎮💸

Donation kiosk + arcade machine for Hackerembassy.  
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
token = "your-bearer-token"

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

### Step 1 — Install RetroArch

```bash
# NixOS
nix-env -iA nixpkgs.retroarch

# Or add to your system configuration.nix:
environment.systemPackages = [ pkgs.retroarch ];
```

Make sure `retroarch` is on `PATH`. By default dramma just calls `retroarch`. You can override this in the config:

```toml
retroarch_command = "/run/current-system/sw/bin/retroarch"
```

### Step 2 — Install cores (emulators)

Each game needs a **libretro core** — a `.so` plugin file that emulates a specific console.

```bash
# Find available cores in nixpkgs:
nix search nixpkgs libretro

# Install a core, e.g. nestopia (NES emulator):
nix-env -iA nixpkgs.libretro.nestopia

# Or add to configuration.nix:
environment.systemPackages = with pkgs.libretro; [
  nestopia      # NES  (Tetris, Super Mario Bros)
  fbneo         # Arcade (Pac-Man, Street Fighter II)
  picodrive     # Sega Genesis (Sonic the Hedgehog)
  dosbox        # DOS (DOOM)
  snes9x        # SNES
];
```

After install, find where the `.so` files ended up:

```bash
find /nix/store -name "nestopia_libretro.so" 2>/dev/null
# → /nix/store/xxxx-nestopia-x.x.x/lib/retroarch/cores/nestopia_libretro.so
```

> **Tip:** Use a stable symlink if you don't want to update the path after every nixpkgs update:
> ```nix
> # In configuration.nix
> environment.etc."retroarch/cores".source = pkgs.symlinkJoin {
>   name = "retroarch-cores";
>   paths = with pkgs.libretro; [ nestopia fbneo picodrive dosbox ];
> } + "/lib/retroarch/cores";
> ```
> Then paths look like `/etc/retroarch/cores/nestopia_libretro.so`.

### Step 3 — Get your ROMs

Put your ROM files somewhere on the machine, e.g. `/home/dramma/roms/`.

```
/home/dramma/roms/
├── tetris.nes
├── pacman.zip
├── sonic.md
└── doom.wad
```

> ROMs are not included for obvious copyright reasons. You know where to find them.

### Step 4 — Configure games in dramma.toml

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

### Step 5 — Test it manually first

Before trusting the machine to do it, test the exact command dramma will run:

```bash
retroarch --fullscreen --kiosk -L /path/to/core_libretro.so /path/to/rom
```

If the game boots correctly, the config is right. If RetroArch opens its menu instead of loading the game, the core or ROM path is wrong.

### Common issues

| Problem | Fix |
|---|---|
| RetroArch opens the menu instead of the game | Wrong core or ROM path. Check paths are correct and the file exists. |
| `retroarch: command not found` | RetroArch not on PATH. Set `retroarch_command` to the full path. |
| Game loads but controls don't work | Configure controller mappings inside RetroArch first, then re-enable kiosk mode. |
| DOOM launches but shows "IWAD not found" | DOOM needs the `.wad` file. Use `doom1.wad` (shareware, freely distributable) or `doom.wad`. |
| Arcade ROMs (Pac-Man, SF2) don't load | FBNeo needs the ROM in a `.zip` file with the exact internal filenames. Use a verified ROM set. |
| Session ends but RetroArch is still open | Check that dramma has permission to `kill` the RetroArch process (usually fine if running as the same user). |

---

## Money → Time conversion

| Inserted | Play time |
|---|---|
| 50 ֏ | 2 min 30 sec |
| 100 ֏ | 5 min |
| 200 ֏ | 10 min |
| 500 ֏ | 25 min |
| 1000 ֏ | 50 min |

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
