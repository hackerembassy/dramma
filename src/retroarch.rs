use crate::config::GameEntry;
use log::{error, info, warn};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

/// Manages a RetroArch subprocess for the arcade game mode.
pub struct RetroArchManager {
    process: Arc<Mutex<Option<Child>>>,
    retroarch_command: String,
}

impl RetroArchManager {
    pub fn new(retroarch_command: &str) -> Self {
        let configured_command = retroarch_command.trim();
        let retroarch_command = without_sudo(configured_command);
        if retroarch_command != configured_command {
            warn!(
                "Ignoring sudo in retroarch_command so RetroArch can use the kiosk user's desktop session"
            );
        }

        Self {
            process: Arc::new(Mutex::new(None)),
            retroarch_command,
        }
    }

    /// Launch RetroArch fullscreen in kiosk mode with the given game entry.
    /// If `game` has empty core/rom paths, RetroArch is launched bare (uses its own last-used config).
    pub fn launch(&self, game: &GameEntry) -> Result<(), String> {
        // Ensure any existing session (and all child processes) is closed first
        self.close();

        let mut process_guard = self.process.lock().unwrap();

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

        #[cfg(unix)]
        {
            cmd.process_group(0);
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

    /// Kill the RetroArch process and any child processes if running.
    pub fn close(&self) {
        let mut process_guard = self.process.lock().unwrap();
        if let Some(ref mut child) = *process_guard {
            let pid = child.id();
            info!("🎮 Closing RetroArch process (PID {})", pid);

            #[cfg(unix)]
            {
                // Kill process group to ensure child processes (e.g. retroarch spawned via sudo) receive SIGKILL
                let _ = Command::new("sudo")
                    .args(["-n", "kill", "-9", &format!("-{}", pid)])
                    .status();
                let _ = Command::new("kill")
                    .args(["-9", &format!("-{}", pid)])
                    .status();
                let _ = Command::new("sudo")
                    .args(["-n", "kill", "-9", &pid.to_string()])
                    .status();
            }

            if let Err(e) = child.kill() {
                error!("Failed to kill RetroArch process handle: {}", e);
            }
            let _ = child.wait();
            info!("🎮 RetroArch process closed");
        }
        *process_guard = None;

        // Fallback cleanup: kill any remaining retroarch processes directly (user and root)
        let _ = Command::new("pkill")
            .args(["-9", "-f", "retroarch"])
            .status();
        let _ = Command::new("sudo")
            .args(["-n", "pkill", "-9", "-f", "retroarch"])
            .status();
    }

    /// Returns `true` if RetroArch was launched and hasn't exited on its own
    /// (crash or otherwise) since. Actually polls the process rather than
    /// just checking launch bookkeeping, so a crash is detected here rather
    /// than only once the full paid session timer elapses.
    pub fn is_running(&self) -> bool {
        let mut process_guard = self.process.lock().unwrap();
        let Some(child) = process_guard.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                info!("🎮 RetroArch exited on its own: {status}");
                *process_guard = None;
                false
            }
            Err(e) => {
                error!("Failed to check RetroArch process status: {}", e);
                true
            }
        }
    }
}

fn without_sudo(command: &str) -> String {
    let command = command.trim();
    if command == "sudo" {
        return "retroarch".to_string();
    }
    let Some(rest) = command.strip_prefix("sudo ") else {
        return command.to_string();
    };

    let command = rest.trim_start();
    if command.is_empty() {
        "retroarch".to_string()
    } else {
        command.to_string()
    }
}

impl Drop for RetroArchManager {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::without_sudo;

    #[test]
    fn keeps_direct_retroarch_command() {
        assert_eq!(
            without_sudo("retroarch --appendconfig=/tmp/kiosk.cfg"),
            "retroarch --appendconfig=/tmp/kiosk.cfg"
        );
    }

    #[test]
    fn strips_sudo_from_retroarch_command() {
        assert_eq!(without_sudo("sudo retroarch"), "retroarch");
    }
}
