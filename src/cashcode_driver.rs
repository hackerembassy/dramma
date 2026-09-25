//! CashCode connection lifecycle, separate from the UI and serial protocol.

use crate::cashcode::{BillEvent, CashCode, CashCodeError};
use log::{error, info};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum CashCodeCommand {
    Enable,
    Disable,
    Reset,
}

#[derive(Default)]
struct ControlState {
    enabled: bool,
    reset_requested: bool,
}

impl ControlState {
    fn apply(&mut self, command: CashCodeCommand) {
        match command {
            CashCodeCommand::Enable => self.enabled = true,
            CashCodeCommand::Disable => self.enabled = false,
            CashCodeCommand::Reset => {
                self.enabled = false;
                self.reset_requested = true;
            }
        }
    }

    /// Read all queued commands before applying an acceptance state to hardware.
    /// Returns false when the UI has shut down.
    fn drain(&mut self, commands: &Receiver<CashCodeCommand>) -> bool {
        loop {
            match commands.try_recv() {
                Ok(command) => self.apply(command),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn wait(&mut self, commands: &Receiver<CashCodeCommand>, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            match commands.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => {
                    self.apply(command);
                    // Diagnostics reset also allows an immediate reconnect.
                    if self.reset_requested {
                        return true;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

struct Timing {
    reconnect: Duration,
    reset: Duration,
    initialization_poll: Duration,
    poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            reconnect: Duration::from_secs(5),
            reset: Duration::from_secs(5),
            initialization_poll: Duration::from_millis(200),
            poll: Duration::from_millis(400),
        }
    }
}

// The device boundary lets recovery be exercised without serial hardware.
trait Device {
    fn reset(&mut self) -> Result<(), CashCodeError>;
    fn set_enabled(&mut self, enabled: bool) -> Result<(), CashCodeError>;
    fn poll(&mut self) -> Result<Option<BillEvent>, CashCodeError>;
    fn total_amount(&self) -> Result<i32, CashCodeError>;
}

impl Device for CashCode {
    fn reset(&mut self) -> Result<(), CashCodeError> {
        self.reset()
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<(), CashCodeError> {
        if enabled {
            self.enable()
        } else {
            self.disable()
        }
    }

    fn poll(&mut self) -> Result<Option<BillEvent>, CashCodeError> {
        self.poll()
    }

    fn total_amount(&self) -> Result<i32, CashCodeError> {
        self.get_total_amount()
    }
}

pub fn run(
    port_path: &str,
    db_path: &str,
    events: Sender<BillEvent>,
    commands: Receiver<CashCodeCommand>,
) -> Result<(), CashCodeError> {
    run_driver(
        || CashCode::new(port_path, db_path),
        events,
        commands,
        Timing::default(),
    )
}

fn run_driver<D: Device>(
    mut connect: impl FnMut() -> Result<D, CashCodeError>,
    events: Sender<BillEvent>,
    commands: Receiver<CashCodeCommand>,
    timing: Timing,
) -> Result<(), CashCodeError> {
    let mut state = ControlState::default();
    loop {
        if !state.drain(&commands) {
            return Ok(());
        }
        // Every new connection is reset, including a diagnostics reset.
        state.reset_requested = false;
        if events
            .send(BillEvent::Status("Connecting...".into(), 0))
            .is_err()
        {
            return Ok(());
        }

        // The session owns the device, so its failed port is closed before
        // waiting or opening a new handle (serial ports are opened exclusively).
        match run_session(&mut connect, &events, &commands, &mut state, &timing) {
            Ok(SessionEnd::Shutdown) => return Ok(()),
            Ok(SessionEnd::Reset) => continue,
            Err(error) => {
                if !matches!(error, CashCodeError::Io(_) | CashCodeError::SerialPort(_)) {
                    let _ = events.send(BillEvent::Status(error.to_string(), 3));
                    return Err(error);
                }
                error!(
                    "CashCode connection failed: {}; reconnecting in {:?}",
                    error, timing.reconnect
                );
                if events
                    .send(BillEvent::Status(
                        format!(
                            "Disconnected: {} · reconnecting in {:?}",
                            error, timing.reconnect
                        ),
                        3,
                    ))
                    .is_err()
                    || !state.wait(&commands, timing.reconnect)
                {
                    return Ok(());
                }
            }
        }
    }
}

enum SessionEnd {
    Shutdown,
    Reset,
}

fn report_state(device: &impl Device, events: &Sender<BillEvent>, enabled: bool) -> bool {
    let total = device.total_amount().unwrap_or(0);
    events
        .send(BillEvent::Status(
            format!(
                "{} · {} ֏ total",
                if enabled { "Enabled" } else { "Disabled" },
                total
            ),
            1,
        ))
        .is_ok()
}

fn run_session<D: Device>(
    connect: &mut impl FnMut() -> Result<D, CashCodeError>,
    events: &Sender<BillEvent>,
    commands: &Receiver<CashCodeCommand>,
    state: &mut ControlState,
    timing: &Timing,
) -> Result<SessionEnd, CashCodeError> {
    let mut device = connect()?;
    if events
        .send(BillEvent::Status("Resetting...".into(), 0))
        .is_err()
    {
        return Ok(SessionEnd::Shutdown);
    }
    device.reset()?;
    thread::sleep(timing.reset);

    for _ in 0..2 {
        // Initialization polls can also contain bill events; do not discard them.
        if let Some(event) = device.poll()?
            && events.send(event).is_err()
        {
            return Ok(SessionEnd::Shutdown);
        }
        thread::sleep(timing.initialization_poll);
    }

    let mut applied_enabled = None;
    loop {
        // Include commands queued during reset/reconnect before enabling again.
        if !state.drain(commands) {
            return Ok(SessionEnd::Shutdown);
        }
        if state.reset_requested {
            return Ok(SessionEnd::Reset);
        }
        if applied_enabled != Some(state.enabled) {
            device.set_enabled(state.enabled)?;
            applied_enabled = Some(state.enabled);
            info!(
                "CashCode bill acceptance {}",
                if state.enabled { "enabled" } else { "disabled" }
            );
            if !report_state(&device, events, state.enabled) {
                return Ok(SessionEnd::Shutdown);
            }
        }

        if let Some(event) = device.poll()? {
            let accepted = matches!(event, BillEvent::Accepted(_));
            if matches!(event, BillEvent::StackerReplaced) {
                // Reapply the UI's intent instead of unconditionally enabling.
                applied_enabled = None;
            }
            if events.send(event).is_err()
                || (accepted && !report_state(&device, events, state.enabled))
            {
                return Ok(SessionEnd::Shutdown);
            }
        }

        if !state.wait(commands, timing.poll) {
            return Ok(SessionEnd::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cashcode::BillNominal;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;
    use std::sync::mpsc::channel;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Operation {
        Reset,
        SetEnabled(bool),
        Poll,
    }

    struct Step {
        operation: Operation,
        result: Result<Option<BillEvent>, CashCodeError>,
        commands: Vec<CashCodeCommand>,
        shutdown: bool,
    }

    impl Step {
        fn ok(operation: Operation) -> Self {
            Self {
                operation,
                result: Ok(None),
                commands: vec![],
                shutdown: false,
            }
        }

        fn broken_pipe(operation: Operation) -> Self {
            Self {
                result: Err(io::Error::from(io::ErrorKind::BrokenPipe).into()),
                ..Self::ok(operation)
            }
        }

        fn shutdown() -> Self {
            Self {
                result: Ok(Some(BillEvent::Accepted(BillNominal::Dram1000))),
                shutdown: true,
                ..Self::ok(Operation::Poll)
            }
        }
    }

    type CommandSender = Rc<RefCell<Option<Sender<CashCodeCommand>>>>;

    struct FakeDevice {
        steps: Rc<RefCell<VecDeque<Step>>>,
        commands: CommandSender,
        lifecycle: Rc<RefCell<Vec<&'static str>>>,
    }

    impl FakeDevice {
        fn perform(&mut self, operation: Operation) -> Result<Option<BillEvent>, CashCodeError> {
            let step = self
                .steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected device operation");
            assert_eq!(operation, step.operation);
            for command in step.commands {
                self.commands
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .send(command)
                    .unwrap();
            }
            if step.shutdown {
                self.commands.borrow_mut().take();
            }
            step.result
        }
    }

    impl Device for FakeDevice {
        fn reset(&mut self) -> Result<(), CashCodeError> {
            self.perform(Operation::Reset).map(|_| ())
        }

        fn set_enabled(&mut self, enabled: bool) -> Result<(), CashCodeError> {
            self.perform(Operation::SetEnabled(enabled)).map(|_| ())
        }

        fn poll(&mut self) -> Result<Option<BillEvent>, CashCodeError> {
            self.perform(Operation::Poll)
        }

        fn total_amount(&self) -> Result<i32, CashCodeError> {
            Ok(1000)
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.lifecycle.borrow_mut().push("close");
        }
    }

    fn initialize(enabled: bool) -> Vec<Step> {
        vec![
            Step::ok(Operation::Reset),
            Step::ok(Operation::Poll),
            Step::ok(Operation::Poll),
            Step::ok(Operation::SetEnabled(enabled)),
        ]
    }

    fn recovered(enabled: bool) -> Vec<Step> {
        let mut steps = initialize(enabled);
        steps.push(Step::shutdown());
        steps
    }

    fn simulate(
        initial_commands: Vec<CashCodeCommand>,
        attempts: Vec<Result<Vec<Step>, CashCodeError>>,
    ) -> (Vec<BillEvent>, Vec<&'static str>) {
        let (event_tx, event_rx) = channel();
        let (cmd_tx, cmd_rx) = channel();
        for command in initial_commands {
            cmd_tx.send(command).unwrap();
        }
        let commands = Rc::new(RefCell::new(Some(cmd_tx)));
        let lifecycle = Rc::new(RefCell::new(vec![]));
        let mut attempts: VecDeque<_> = attempts.into();
        let mut sessions = vec![];
        run_driver(
            || {
                lifecycle.borrow_mut().push("open");
                let steps = attempts.pop_front().expect("unexpected reconnect")?;
                let steps = Rc::new(RefCell::new(VecDeque::from(steps)));
                sessions.push(steps.clone());
                Ok(FakeDevice {
                    steps,
                    commands: commands.clone(),
                    lifecycle: lifecycle.clone(),
                })
            },
            event_tx,
            cmd_rx,
            Timing {
                reconnect: Duration::ZERO,
                reset: Duration::ZERO,
                initialization_poll: Duration::ZERO,
                poll: Duration::ZERO,
            },
        )
        .unwrap();
        assert!(attempts.is_empty(), "driver stopped before reconnecting");
        assert!(sessions.iter().all(|steps| steps.borrow().is_empty()));
        let lifecycle = lifecycle.borrow().clone();
        (event_rx.try_iter().collect(), lifecycle)
    }

    #[test]
    fn broken_pipe_reopens_port_after_reset_initialization_enable_or_poll_failure() {
        for failure_index in 0..5 {
            let mut steps = initialize(true);
            steps.push(Step::ok(Operation::Poll));
            steps.truncate(failure_index + 1);
            let operation = steps.last().unwrap().operation;
            *steps.last_mut().unwrap() = Step::broken_pipe(operation);

            let (events, lifecycle) = simulate(
                vec![CashCodeCommand::Enable],
                vec![Ok(steps), Ok(recovered(true))],
            );
            assert_eq!(lifecycle, ["open", "close", "open", "close"]);
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, BillEvent::Accepted(_)))
                    .count(),
                1
            );
            assert!(
                events.iter().any(
                    |e| matches!(e, BillEvent::Status(text, 3) if text.contains("reconnecting"))
                )
            );
            assert!(
                matches!(events.last(), Some(BillEvent::Status(text, 1)) if text.starts_with("Enabled"))
            );
        }
    }

    #[test]
    fn unavailable_port_at_startup_is_retried() {
        let unavailable = || {
            CashCodeError::SerialPort(serialport::Error::new(
                serialport::ErrorKind::NoDevice,
                "USB adapter unplugged",
            ))
        };
        let (_, lifecycle) = simulate(
            vec![],
            vec![Err(unavailable()), Err(unavailable()), Ok(recovered(false))],
        );
        assert_eq!(lifecycle, ["open", "open", "open", "close"]);
    }

    #[test]
    fn failed_disable_is_retried_on_new_connection() {
        let mut steps = initialize(true);
        steps.push(Step {
            commands: vec![CashCodeCommand::Disable],
            ..Step::ok(Operation::Poll)
        });
        steps.push(Step::broken_pipe(Operation::SetEnabled(false)));
        simulate(
            vec![CashCodeCommand::Enable],
            vec![Ok(steps), Ok(recovered(false))],
        );
    }

    #[test]
    fn commands_during_outage_and_initialization_override_previous_enable() {
        let mut steps = initialize(true);
        steps.push(Step {
            commands: vec![CashCodeCommand::Disable, CashCodeCommand::Enable],
            ..Step::broken_pipe(Operation::Poll)
        });
        let mut next = recovered(false);
        next[0].commands.push(CashCodeCommand::Disable);
        simulate(vec![CashCodeCommand::Enable], vec![Ok(steps), Ok(next)]);
    }

    #[test]
    fn diagnostics_reset_reopens_port_and_leaves_acceptance_disabled() {
        let mut steps = initialize(true);
        steps.push(Step {
            commands: vec![CashCodeCommand::Reset],
            ..Step::ok(Operation::Poll)
        });
        let (_, lifecycle) = simulate(
            vec![CashCodeCommand::Enable],
            vec![Ok(steps), Ok(recovered(false))],
        );
        assert_eq!(lifecycle, ["open", "close", "open", "close"]);
    }

    #[test]
    fn stacker_replacement_preserves_disabled_state() {
        let mut steps = initialize(false);
        steps.push(Step {
            result: Ok(Some(BillEvent::StackerReplaced)),
            ..Step::ok(Operation::Poll)
        });
        steps.push(Step::ok(Operation::SetEnabled(false)));
        steps.push(Step::shutdown());
        simulate(vec![], vec![Ok(steps)]);
    }

    #[test]
    fn initialization_bill_event_is_forwarded_once() {
        let mut steps = recovered(false);
        steps[1].result = Ok(Some(BillEvent::Accepted(BillNominal::Dram5000)));
        let (events, _) = simulate(vec![], vec![Ok(steps)]);
        let accepted: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                BillEvent::Accepted(nominal) => Some(*nominal),
                _ => None,
            })
            .collect();
        assert_eq!(accepted, [BillNominal::Dram5000, BillNominal::Dram1000]);
    }

    #[test]
    fn closed_command_channel_stops_retries() {
        let (event_tx, _event_rx) = channel();
        let (cmd_tx, cmd_rx) = channel();
        drop(cmd_tx);
        run_driver::<FakeDevice>(
            || panic!("must not open a port after UI shutdown"),
            event_tx,
            cmd_rx,
            Timing::default(),
        )
        .unwrap();
    }
}
