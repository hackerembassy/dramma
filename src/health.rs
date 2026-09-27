//! Device availability shared by the drivers, kiosk UI, Home Assistant sensor and watchdog.

use log::error;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const STALE_AFTER: Duration = Duration::from_secs(30);
const REBOOT_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub enum Acceptor {
    CashCode = 0,
    CcTalk = 1,
}

impl Acceptor {
    const ALL: [Self; 2] = [Self::CashCode, Self::CcTalk];

    fn name(self) -> &'static str {
        match self {
            Self::CashCode => "cashcode",
            Self::CcTalk => "cctalk",
        }
    }
}

struct DeviceState {
    error: Option<String>,
    unavailable_since: Option<Instant>,
    last_response: Option<Instant>,
}

struct State {
    devices: [DeviceState; 2],
    last_reboot_attempt: Option<Instant>,
    reboot_requested: bool,
    reboot_error: Option<String>,
    maintenance_since: Option<Instant>,
}

#[derive(Clone)]
pub struct Health {
    state: Arc<Mutex<State>>,
    restart_after: Duration,
}

#[derive(Clone)]
pub struct DeviceHealth {
    health: Health,
    device: Acceptor,
}

#[derive(Debug, Serialize)]
pub struct HealthError {
    pub component: &'static str,
    pub message: String,
    pub unavailable_for_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct HealthSnapshot {
    pub status: &'static str,
    pub errors: Vec<HealthError>,
    pub restart_in_secs: Option<u64>,
    pub restart_requested: bool,
}

impl HealthSnapshot {
    pub fn healthy(&self) -> bool {
        self.errors.is_empty() && !self.restart_requested
    }
}

impl Health {
    pub fn new(restart_after: Duration) -> Self {
        Self::new_at(restart_after, Instant::now())
    }

    fn new_at(restart_after: Duration, now: Instant) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                devices: std::array::from_fn(|_| DeviceState {
                    error: Some("Initializing; no successful poll yet".into()),
                    unavailable_since: Some(now),
                    last_response: None,
                }),
                last_reboot_attempt: None,
                reboot_requested: false,
                reboot_error: None,
                maintenance_since: None,
            })),
            restart_after,
        }
    }

    pub fn device(&self, device: Acceptor) -> DeviceHealth {
        DeviceHealth {
            health: self.clone(),
            device,
        }
    }

    /// Manually forces the machine unhealthy, independent of acceptor state.
    /// Does not affect the reboot watchdog, which only watches acceptors.
    pub fn set_maintenance_mode(&self, enabled: bool) {
        self.set_maintenance_mode_at(enabled, Instant::now());
    }

    fn set_maintenance_mode_at(&self, enabled: bool, now: Instant) {
        let mut state = self.state.lock().unwrap();
        if enabled {
            state.maintenance_since.get_or_insert(now);
        } else {
            state.maintenance_since = None;
        }
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> HealthSnapshot {
        let mut state = self.state.lock().unwrap();
        Self::expire_stale(&mut state, now);
        let mut errors = Vec::new();
        let mut restart_in = None;
        for device in Acceptor::ALL {
            let device_state = &state.devices[device as usize];
            if let Some(message) = &device_state.error {
                let elapsed =
                    now.saturating_duration_since(device_state.unavailable_since.unwrap());
                errors.push(HealthError {
                    component: device.name(),
                    message: message.clone(),
                    unavailable_for_secs: elapsed.as_secs(),
                });
                if !self.restart_after.is_zero() {
                    let remaining = self.restart_after.saturating_sub(elapsed);
                    restart_in =
                        Some(restart_in.map_or(remaining, |old: Duration| old.min(remaining)));
                }
            }
        }
        if let Some(message) = &state.reboot_error {
            errors.push(HealthError {
                component: "reboot",
                message: message.clone(),
                unavailable_for_secs: 0,
            });
        }
        if let Some(since) = state.maintenance_since {
            errors.push(HealthError {
                component: "maintenance",
                message: "Maintenance mode enabled from diagnostics".to_string(),
                unavailable_for_secs: now.saturating_duration_since(since).as_secs(),
            });
        }
        HealthSnapshot {
            status: if errors.is_empty() && !state.reboot_requested {
                "ok"
            } else {
                "error"
            },
            errors,
            restart_in_secs: restart_in.map(|remaining| remaining.as_secs()),
            restart_requested: state.reboot_requested,
        }
    }

    fn expire_stale(state: &mut State, now: Instant) {
        for device in &mut state.devices {
            if device.error.is_none()
                && let Some(last_response) = device.last_response
                && now.saturating_duration_since(last_response) >= STALE_AFTER
            {
                device.error =
                    Some("No successful poll for 30 seconds; device worker may be stalled".into());
                device.unavailable_since = Some(last_response + STALE_AFTER);
            }
        }
    }

    /// The clock and reboot action are supplied separately so tests never reboot a host.
    fn watchdog_tick(&self, now: Instant, reboot: impl FnOnce() -> Result<(), String>) {
        {
            let mut state = self.state.lock().unwrap();
            Self::expire_stale(&mut state, now);
            if self.restart_after.is_zero()
                || state.reboot_requested
                || state
                    .last_reboot_attempt
                    .is_some_and(|last| now.saturating_duration_since(last) < REBOOT_RETRY)
                || !state.devices.iter().any(|device| {
                    device.unavailable_since.is_some_and(|since| {
                        now.saturating_duration_since(since) >= self.restart_after
                    })
                })
            {
                return;
            }
            state.last_reboot_attempt = Some(now);
        }

        error!(
            "Acceptor recovery timeout reached; requesting computer restart: {:?}",
            self.snapshot_at(now).errors
        );
        let result = reboot();
        let mut state = self.state.lock().unwrap();
        match result {
            Ok(()) => {
                state.reboot_requested = true;
                state.reboot_error = None;
            }
            Err(message) => {
                error!("Computer restart failed: {}", message);
                state.reboot_error = Some(message);
            }
        }
    }

    pub fn start_watchdog(&self) {
        let health = self.clone();
        thread::spawn(move || {
            loop {
                health.watchdog_tick(Instant::now(), reboot_computer);
                thread::sleep(Duration::from_secs(1));
            }
        });
    }
}

impl DeviceHealth {
    pub fn ready(&self) {
        self.update(None, Instant::now());
    }

    pub fn unavailable(&self, message: impl Into<String>) {
        self.update(Some(message.into()), Instant::now());
    }

    fn update(&self, error: Option<String>, now: Instant) {
        let mut state = self.health.state.lock().unwrap();
        let device = &mut state.devices[self.device as usize];
        if error.is_some() {
            // Retrying/opening/resetting must not restart the outage timer.
            device.unavailable_since.get_or_insert(now);
        } else {
            device.unavailable_since = None;
            device.last_response = Some(now);
        }
        device.error = error;
        if state.devices.iter().all(|device| device.error.is_none()) {
            state.reboot_error = None;
        }
    }

    pub fn worker_guard(&self) -> WorkerGuard {
        WorkerGuard(self.clone())
    }
}

pub struct WorkerGuard(DeviceHealth);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let mut state = self.0.health.state.lock().unwrap();
        let device = &mut state.devices[self.0.device as usize];
        let message = device.error.take().map_or_else(
            || "Device worker stopped".to_string(),
            |error| format!("Device worker stopped: {error}"),
        );
        device.error = Some(message);
        device.unavailable_since.get_or_insert_with(Instant::now);
    }
}

fn reboot_computer() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // Matches the narrowly scoped sudoers rule installed by deployment.
        // Never prompt for a password or force past shutdown inhibitors.
        let output = std::process::Command::new("sudo")
            .args(["-n", "/usr/bin/systemctl", "--no-block", "reboot"])
            .output()
            .map_err(|error| format!("Could not request reboot: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Reboot command exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
    #[cfg(not(target_os = "linux"))]
    Err("Automatic reboot is only supported on Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn healthy(health: &Health, now: Instant) {
        for device in Acceptor::ALL {
            health.device(device).update(None, now);
        }
    }

    #[test]
    fn starts_unhealthy_until_both_acceptors_respond() {
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(300), now);
        assert_eq!(health.snapshot_at(now).errors.len(), 2);
        health.device(Acceptor::CashCode).update(None, now);
        assert_eq!(health.snapshot_at(now).errors[0].component, "cctalk");
        health.device(Acceptor::CcTalk).update(None, now);
        assert!(health.snapshot_at(now).healthy());
    }

    #[test]
    fn retries_do_not_extend_outage_and_either_device_can_trigger_reboot() {
        for failed in Acceptor::ALL {
            let now = Instant::now();
            let health = Health::new_at(Duration::from_secs(300), now);
            healthy(&health, now);
            let device = health.device(failed);
            device.update(Some("disconnected".into()), now + Duration::from_secs(1));
            device.update(Some("retrying".into()), now + Duration::from_secs(299));
            let calls = Cell::new(0);
            health.watchdog_tick(now + Duration::from_secs(300), || {
                calls.set(calls.get() + 1);
                Ok(())
            });
            assert_eq!(calls.get(), 0);
            health.watchdog_tick(now + Duration::from_secs(301), || {
                calls.set(calls.get() + 1);
                Ok(())
            });
            health.watchdog_tick(now + Duration::from_secs(400), || {
                calls.set(calls.get() + 1);
                Ok(())
            });
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn recovery_cancels_reboot_and_next_outage_gets_a_fresh_deadline() {
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(300), now);
        healthy(&health, now + Duration::from_secs(299));
        health.watchdog_tick(now + Duration::from_secs(300), || panic!("recovered"));
        health
            .device(Acceptor::CcTalk)
            .update(Some("missing again".into()), now + Duration::from_secs(300));
        assert_eq!(
            health
                .snapshot_at(now + Duration::from_secs(301))
                .restart_in_secs,
            Some(299)
        );
    }

    #[test]
    fn hung_worker_becomes_unavailable_and_reboots_after_deadline() {
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(300), now);
        healthy(&health, now);
        assert!(
            health.snapshot_at(now + STALE_AFTER).errors[0]
                .message
                .contains("stalled")
        );
        let called = Cell::new(false);
        health.watchdog_tick(now + STALE_AFTER + Duration::from_secs(300), || {
            called.set(true);
            Ok(())
        });
        assert!(called.get());
    }

    #[test]
    fn failed_reboot_is_reported_and_retried_with_backoff() {
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(1), now);
        health.watchdog_tick(now + Duration::from_secs(1), || {
            Err("permission denied".into())
        });
        assert!(
            health
                .snapshot_at(now + Duration::from_secs(1))
                .errors
                .iter()
                .any(|error| error.component == "reboot")
        );
        health.watchdog_tick(now + Duration::from_secs(2), || panic!("retry too soon"));
        health.watchdog_tick(now + Duration::from_secs(61), || Ok(()));
        assert!(
            health
                .snapshot_at(now + Duration::from_secs(61))
                .restart_requested
        );
    }

    #[test]
    fn zero_timeout_disables_reboot_but_keeps_errors_visible() {
        let now = Instant::now();
        let health = Health::new_at(Duration::ZERO, now);
        health.watchdog_tick(now + Duration::from_secs(3600), || panic!("disabled"));
        let snapshot = health.snapshot_at(now);
        assert!(!snapshot.healthy());
        assert_eq!(snapshot.restart_in_secs, None);
    }

    #[test]
    fn stopped_worker_is_unhealthy_and_preserves_failure_reason() {
        let health = Health::new(Duration::ZERO);
        healthy(&health, Instant::now());
        let device = health.device(Acceptor::CashCode);
        let worker = device.worker_guard();
        device.unavailable("database unavailable");
        drop(worker);
        let snapshot = health.snapshot();
        assert!(!snapshot.healthy());
        assert!(
            snapshot.errors[0]
                .message
                .contains("Device worker stopped: database unavailable")
        );
    }

    #[test]
    fn maintenance_mode_overrides_otherwise_healthy_devices() {
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(300), now);
        healthy(&health, now);
        assert!(health.snapshot_at(now).healthy());

        health.set_maintenance_mode_at(true, now);
        let snapshot = health.snapshot_at(now + Duration::from_secs(5));
        assert!(!snapshot.healthy());
        assert_eq!(snapshot.errors.len(), 1);
        assert_eq!(snapshot.errors[0].component, "maintenance");
        assert_eq!(snapshot.errors[0].unavailable_for_secs, 5);

        health.set_maintenance_mode(false);
        assert!(health.snapshot_at(now + Duration::from_secs(5)).healthy());
    }

    #[test]
    fn maintenance_mode_never_triggers_the_reboot_watchdog() {
        // restart_after is kept under STALE_AFTER so this only exercises
        // maintenance mode, not the separate stalled-worker outage path.
        let now = Instant::now();
        let health = Health::new_at(Duration::from_secs(10), now);
        healthy(&health, now);
        health.set_maintenance_mode_at(true, now);

        health.watchdog_tick(now + Duration::from_secs(20), || {
            panic!("maintenance mode must not reboot the machine")
        });
        assert!(
            !health
                .snapshot_at(now + Duration::from_secs(20))
                .restart_requested
        );
    }
}
