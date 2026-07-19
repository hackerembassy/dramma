use crate::config::GameEntry;
use log::{error, info};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

/// Manages a RetroArch subprocess for the arcade game mode.
pub struct RetroArchManager {
    process: Arc<Mutex<Option<Child>>>,
    retroarch_command: String,
}

impl RetroArchManager {
    pub fn new(retroarch_command: &str) -> Self {
        Self {
            process: Arc::new(Mutex::new(None)),
            retroarch_command: retroarch_command.to_string(),
        }
    }

    /// Launch RetroArch fullscreen in kiosk mode with the given game entry.
    /// If `game` has empty core/rom paths, RetroArch is launched bare (uses its own last-used config).
    pub fn launch(&self, game: &GameEntry) -> Result<(), String> {
        let mut process_guard = self.process.lock().unwrap();

        // Kill any already-running session
        if let Some(ref mut child) = *process_guard {
            info!("🎮 Killing existing RetroArch process before relaunch");
            let _ = child.kill();
            let _ = child.wait();
        }

        info!(
            "🎮 Launching RetroArch: game=\"{}\" core=\"{}\" rom=\"{}\"",
            game.name, game.core, game.rom
        );

        let mut parts = self.retroarch_command.split_whitespace();
        let program = parts.next().unwrap_or("retroarch");
        let mut cmd = Command::new(program);
        
        for arg in parts {
            cmd.arg(arg);
        }
        
        cmd.arg("--fullscreen");

        if !game.core.is_empty() {
            cmd.arg("--libretro").arg(&game.core);
        }
        if !game.rom.is_empty() {
            cmd.arg(&game.rom);
        }

        match cmd.spawn() {
            Ok(child) => {
                info!("🎮 RetroArch launched with PID {}", child.id());
                *process_guard = Some(child);
                Ok(())
            }
            Err(e) => {
                let msg = format!(
                    "Failed to launch RetroArch (command: \"{}\"): {}",
                    self.retroarch_command, e
                );
                error!("{}", msg);
                Err(msg)
            }
        }
    }

    /// Kill the RetroArch process if running.
    pub fn close(&self) {
        let mut process_guard = self.process.lock().unwrap();
        if let Some(ref mut child) = *process_guard {
            info!("🎮 Closing RetroArch process");
            if let Err(e) = child.kill() {
                error!("Failed to kill RetroArch: {}", e);
            } else {
                let _ = child.wait();
                info!("🎮 RetroArch process closed");
            }
        }
        *process_guard = None;
    }

    /// Returns `true` if RetroArch is currently running.
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.process.lock().unwrap().is_some()
    }
}

impl Drop for RetroArchManager {
    fn drop(&mut self) {
        self.close();
    }
}
