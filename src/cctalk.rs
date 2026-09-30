//! ccTalk coin acceptor integration using a direct serial port transport.
//!
//! This module uses `tokio-serial` to communicate with the coin acceptor
//! directly — no socat bridge required. The custom `CcTalkSerialTransport`
//! mirrors the logic of `CcTalkTokioTransport` from `cc_talk_tokio_host` but
//! speaks to a `SerialStream` instead of a Unix socket.
//!
//! Connection loss is detected via consecutive poll errors and triggers an
//! automatic reconnect with a configurable delay. The enabled/disabled state
//! is preserved across reconnects.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use crate::health::DeviceHealth;
use cc_talk_core::cc_talk::{
    Address, Category, ChecksumType, CoinEvent, DATA_LENGTH_OFFSET, Device, MAX_BLOCK_LENGTH,
    Packet, deserializer::deserialize, serializer::serialize,
};
use cc_talk_host::device::device_commands::RequestCoinIdCommand;
use cc_talk_tokio_host::{
    device::{base::DeviceCommon, coin_validator::CoinValidator},
    transport::tokio_transport::{TransportError, TransportMessage},
};
use log::{error, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::timeout;
use tokio_serial::SerialStream;

/// Baud rate used by ccTalk devices (fixed by the spec).
const CCTALK_BAUD: u32 = 9600;

/// When `true`, bytes written to the serial port are echoed back and consumed
/// before reading the device response.  This is needed for RS-485 half-duplex
/// wiring; set to `false` for RS-232 full-duplex.
const ECHO: bool = true;

/// Delay between reconnect attempts when the serial connection is lost.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Value for `serial_port` that triggers automatic discovery across all
/// available USB serial ports.
const AUTO_PORT: &str = "auto";

/// Number of consecutive poll errors before the connection is declared lost
/// and a reconnect is attempted.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Extra pause after a coin credit event, giving the coin mechanism time to
/// settle before the next poll.  Without this the next serial read can catch
/// electrical noise from the solenoid/motor and trigger a framing error.
const POST_CREDIT_DELAY: Duration = Duration::from_millis(800);

/// One-based positions listed in this file are inhibited. Missing or empty
/// means every programmed coin slot is enabled.
pub const DEFAULT_COIN_SLOT_STATE_PATH: &str = "data/cctalk_disabled_coin_slots";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoinAcceptorCommand {
    Enable,
    Disable,
    SetSlotEnabled {
        position: u8,
        enabled: bool,
    },
    /// Triggers a USB-level re-enumeration of the coin acceptor (via udevadm)
    /// and forces a reconnect.  Solenoids are clicked once the device is found.
    Reenumerate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinSlotInfo {
    pub position: u8,
    pub coin_id: String,
    pub value: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub enum CoinAcceptorEvent {
    /// Coin accepted; value in AMD (smallest unit).
    Accepted(i32),
    Error(String),
    /// Lifecycle / device-state update for the diagnostics page.
    /// level: 0 = neutral · 1 = ok · 2 = warn · 3 = error
    Status(String, i32),
    /// Complete, position-sorted snapshot of the programmed coin slots.
    Slots(Vec<CoinSlotInfo>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlState {
    accepting: bool,
    /// `true` means the one-based coin position at index + 1 is inhibited.
    coin_inhibits: [bool; 16],
}

impl ControlState {
    fn load(path: &Path) -> Self {
        Self {
            accepting: false,
            coin_inhibits: load_coin_inhibits(path),
        }
    }

    fn set_slot_enabled(&mut self, position: u8, enabled: bool) -> Result<(), String> {
        let Some(index) = position.checked_sub(1).map(usize::from).filter(|&i| i < 16) else {
            return Err(format!("Invalid ccTalk coin slot position: {position}"));
        };
        self.coin_inhibits[index] = !enabled;
        Ok(())
    }
}

fn load_coin_inhibits(path: &Path) -> [bool; 16] {
    let mut inhibits = [false; 16];
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return inhibits,
        Err(error) => {
            error!("Failed to load disabled ccTalk coin slots from {path:?}: {error}");
            return inhibits;
        }
    };

    for (line_index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line.parse::<usize>() {
            Ok(position @ 1..=16) => inhibits[position - 1] = true,
            _ => warn!(
                "Ignoring invalid ccTalk coin slot {:?} on line {} of {path:?}",
                line,
                line_index + 1
            ),
        }
    }
    inhibits
}

fn save_coin_inhibits(path: &Path, inhibits: &[bool; 16]) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let contents = inhibits
        .iter()
        .enumerate()
        .filter_map(|(index, inhibited)| inhibited.then_some(format!("{}\n", index + 1)))
        .collect::<String>();
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    std::fs::rename(temporary, path)
}

fn update_slot_intent(
    state: &mut ControlState,
    position: u8,
    enabled: bool,
    state_path: &Path,
    event_tx: &Sender<CoinAcceptorEvent>,
) -> bool {
    if let Err(message) = state.set_slot_enabled(position, enabled) {
        error!("{message}");
        let _ = event_tx.send(CoinAcceptorEvent::Error(message));
        return false;
    }

    info!(
        "ccTalk coin slot {} {} from diagnostics",
        position,
        if enabled { "enabled" } else { "disabled" }
    );
    if let Err(error) = save_coin_inhibits(state_path, &state.coin_inhibits) {
        let message = format!("Failed to persist coin slot settings: {error}");
        error!("{message}");
        let _ = event_tx.send(CoinAcceptorEvent::Error(message));
    }
    true
}

fn coin_slot_snapshot(
    coin_values: &HashMap<u8, i32>,
    coin_ids: &HashMap<u8, String>,
    inhibits: &[bool; 16],
) -> Vec<CoinSlotInfo> {
    let mut slots = coin_values
        .iter()
        .filter_map(|(&position, &value)| {
            let index = usize::from(position.checked_sub(1)?);
            let &inhibited = inhibits.get(index)?;
            Some(CoinSlotInfo {
                position,
                coin_id: coin_ids
                    .get(&position)
                    .cloned()
                    .unwrap_or_else(|| "configured override".to_string()),
                value,
                enabled: !inhibited,
            })
        })
        .collect::<Vec<_>>();
    slots.sort_by_key(|slot| slot.position);
    slots
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Runs the ccTalk coin-acceptor driver on a dedicated tokio current-thread
/// runtime.  Sends `CoinAcceptorEvent`s back via `event_tx`.
///
/// `coin_overrides` is a list of `[position, amd_value]` pairs that take
/// precedence over the value derived from the device's coin ID strings.
/// Use this when the device has misconfigured coin IDs for one or more slots.
///
/// Automatically reconnects if the serial connection is lost, preserving the
/// last known enabled/disabled state across reconnects.
pub fn run(
    serial_port: String,
    event_tx: Sender<CoinAcceptorEvent>,
    cmd_rx: Receiver<CoinAcceptorCommand>,
    coin_overrides: Vec<[i32; 2]>,
    health: DeviceHealth,
) {
    let _worker = health.worker_guard();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            health.unavailable(format!("Failed to start ccTalk runtime: {e}"));
            error!(
                "Failed to create tokio runtime for ccTalk coin acceptor: {}",
                e
            );
            return;
        }
    };

    rt.block_on(async move {
        let state_path = Path::new(DEFAULT_COIN_SLOT_STATE_PATH);
        let mut state = ControlState::load(state_path);
        let auto = serial_port == AUTO_PORT;
        // Set to true when a Reenumerate command was processed; causes solenoids
        // to be clicked once the next session connects successfully.
        let mut ping_on_connect = false;

        loop {
            // When auto-discovery is enabled, scan for the device before each
            // session.  This handles USB port number changes on reconnect.
            let port = if auto {
                let _ = event_tx.send(CoinAcceptorEvent::Status(
                    "Scanning for ccTalk device...".to_string(),
                    0,
                ));
                loop {
                    match find_cctalk_port().await {
                        Some(p) => break p,
                        None => {
                            health.unavailable("No ccTalk device found");
                            warn!("ccTalk: no device found, retrying in {:?}", RECONNECT_DELAY);
                            let _ = event_tx.send(CoinAcceptorEvent::Status(
                                format!("No device found · retrying in {:?}", RECONNECT_DELAY),
                                2,
                            ));
                            tokio::time::sleep(RECONNECT_DELAY).await;
                        }
                    }
                }
            } else {
                serial_port.clone()
            };

            info!("ccTalk: connecting to {}...", port);
            let _ = event_tx.send(CoinAcceptorEvent::Status("Connecting...".to_string(), 0));
            match run_session(
                &port,
                event_tx.clone(),
                &cmd_rx,
                &mut state,
                &coin_overrides,
                ping_on_connect,
                &health,
            )
            .await
            {
                Ok(()) => {
                    info!("ccTalk: session ended cleanly, exiting");
                    break;
                }
                Err(e) => {
                    health.unavailable(e.to_string());
                    let is_reenumerate = e.to_string() == "reenumerate";
                    ping_on_connect = is_reenumerate;

                    if is_reenumerate {
                        info!("ccTalk: waiting for USB device to settle after re-enumeration...");
                        let _ = event_tx.send(CoinAcceptorEvent::Status(
                            "Re-enumerating USB · waiting...".to_string(),
                            0,
                        ));
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    } else {
                        error!(
                            "ccTalk: connection lost ({}), reconnecting in {:?}",
                            e, RECONNECT_DELAY
                        );
                        let _ = event_tx.send(CoinAcceptorEvent::Status(
                            format!("Disconnected · reconnecting in {:?}", RECONNECT_DELAY),
                            3,
                        ));
                        // Drain queued commands so the latest global and
                        // per-slot intent survives the reconnect delay.
                        while let Ok(cmd) = cmd_rx.try_recv() {
                            match cmd {
                                CoinAcceptorCommand::Enable => state.accepting = true,
                                CoinAcceptorCommand::Disable => state.accepting = false,
                                CoinAcceptorCommand::SetSlotEnabled { position, enabled } => {
                                    update_slot_intent(
                                        &mut state, position, enabled, state_path, &event_tx,
                                    );
                                }
                                CoinAcceptorCommand::Reenumerate => {
                                    ping_on_connect = true;
                                    reenumerate_usb().await;
                                }
                            }
                        }
                        tokio::time::sleep(RECONNECT_DELAY).await;
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Serial transport
// ---------------------------------------------------------------------------

/// Minimal serial transport that processes `TransportMessage`s using tokio-serial.
///
/// Mirrors the logic of `CcTalkTokioTransport` but opens a `SerialStream`
/// instead of a Unix socket, eliminating the socat dependency.
struct CcTalkSerialTransport {
    receiver: tokio_mpsc::Receiver<TransportMessage>,
    serial_port: String,
    rw_timeout: Duration,
}

impl CcTalkSerialTransport {
    fn new(
        receiver: tokio_mpsc::Receiver<TransportMessage>,
        serial_port: String,
        rw_timeout: Duration,
    ) -> Self {
        Self {
            receiver,
            serial_port,
            rw_timeout,
        }
    }

    fn open_port(&self) -> Result<SerialStream, Box<dyn std::error::Error>> {
        let builder = tokio_serial::new(&self.serial_port, CCTALK_BAUD)
            .data_bits(tokio_serial::DataBits::Eight)
            .stop_bits(tokio_serial::StopBits::One)
            .parity(tokio_serial::Parity::None)
            .timeout(self.rw_timeout);

        SerialStream::open(&builder)
            .map_err(|e| format!("Failed to open serial port {}: {}", self.serial_port, e).into())
    }

    async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let port = self.open_port()?;
        self.run_on(port).await
    }

    async fn run_on(mut self, mut port: SerialStream) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            "ccTalk: serial port {} opened at {} baud",
            self.serial_port, CCTALK_BAUD
        );

        let mut send_buf = vec![0u8; MAX_BLOCK_LENGTH];
        let mut recv_buf = vec![0u8; MAX_BLOCK_LENGTH];

        while let Some(msg) = self.receiver.recv().await {
            let result = handle_message(
                &msg,
                &mut send_buf,
                &mut recv_buf,
                self.rw_timeout,
                &mut port,
            )
            .await;
            if result.is_err() {
                // Drain any leftover bytes in the serial input buffer.
                // A framing or timeout error can leave partial data behind;
                // consuming it here prevents the next message from reading
                // stale bytes and getting permanently out of sync.
                drain_input(&mut port).await;
            }
            msg.respond_to.send(result).ok();
        }

        Ok(())
    }
}

/// Discard any bytes sitting in the serial input buffer.
///
/// Called after a transport error to re-synchronise framing. Uses a very
/// short read timeout so it exits quickly once the line goes quiet.
async fn drain_input(port: &mut SerialStream) {
    let mut buf = [0u8; 64];
    let drain_timeout = Duration::from_millis(30);
    loop {
        match timeout(drain_timeout, port.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
        }
    }
}

/// Serialise, send, (optionally consume echo,) receive, and validate one
/// ccTalk message over the serial port.
async fn handle_message(
    msg: &TransportMessage,
    send_buf: &mut [u8],
    recv_buf: &mut [u8],
    rw_timeout: Duration,
    port: &mut SerialStream,
) -> Result<Vec<u8>, TransportError> {
    // --- build & serialise packet ---
    let mut send_pkt = Packet::new(&mut *send_buf);
    send_pkt
        .set_destination(msg.address)
        .map_err(|_| TransportError::BufferOverflow)?;
    send_pkt
        .set_source(1)
        .map_err(|_| TransportError::BufferOverflow)?;
    send_pkt
        .set_header(msg.header)
        .map_err(|_| TransportError::BufferOverflow)?;
    send_pkt
        .set_data(&msg.data)
        .map_err(|_| TransportError::BufferOverflow)?;

    let device = Device::new(msg.address, Category::Unknown, msg.checksum_type);
    serialize(&device, &mut send_pkt).map_err(|_| TransportError::PacketCreationError)?;

    let pkt_len = send_pkt.get_logical_size();

    // --- write ---
    timeout(rw_timeout, port.write_all(&send_buf[..pkt_len]))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|_| TransportError::SocketWriteError)?;
    port.flush()
        .await
        .map_err(|_| TransportError::SocketWriteError)?;

    // --- consume echo (RS-485 half-duplex) ---
    if ECHO {
        timeout(rw_timeout, port.read_exact(&mut send_buf[..pkt_len]))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::SocketReadError)?;
    }

    // --- read response header (5 bytes) ---
    timeout(rw_timeout, port.read_exact(&mut recv_buf[..5]))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|_| TransportError::SocketReadError)?;

    // --- read remaining data bytes (length from packet field) ---
    let data_len = recv_buf[DATA_LENGTH_OFFSET] as usize;
    if data_len > 0 {
        timeout(rw_timeout, port.read_exact(&mut recv_buf[5..5 + data_len]))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::SocketReadError)?;
    }

    // --- validate checksum ---
    let total_len = 5 + data_len;
    let mut recv_pkt = Packet::new(&mut recv_buf[..total_len]);
    deserialize(&mut recv_pkt, msg.checksum_type).map_err(|_| TransportError::ChecksumError)?;

    Ok(recv_buf[..total_len].to_vec())
}

// ---------------------------------------------------------------------------
// Automatic port discovery
// ---------------------------------------------------------------------------

/// Asks udev to re-add Silicon Labs CP210x USB-serial devices.  This causes
/// the kernel to re-bind the cp210x driver and re-create `/dev/ttyUSBx` nodes,
/// which is useful after removing `brltty` or after a device replug.
async fn reenumerate_usb() {
    info!("ccTalk: triggering USB re-enumeration for cp210x (idVendor=10c4)...");
    let status = tokio::process::Command::new("udevadm")
        .args(["trigger", "--action=add", "--attr-match=idVendor=10c4"])
        .status()
        .await;
    match status {
        Ok(s) if s.success() => info!("ccTalk: udevadm trigger succeeded"),
        Ok(s) => warn!("ccTalk: udevadm trigger exited with {}", s),
        Err(e) => warn!("ccTalk: udevadm trigger failed: {}", e),
    }
}

/// Scans available serial ports and returns the name of the first one that
/// responds to a ccTalk SimplePoll sent to the coin-acceptor address.
///
/// On Linux only USB serial converters (`/dev/ttyUSB*`, `/dev/ttyACM*`) are
/// probed to avoid disturbing unrelated devices.
async fn find_cctalk_port() -> Option<String> {
    let ports = match tokio_serial::available_ports() {
        Ok(p) => p,
        Err(e) => {
            warn!("ccTalk: cannot enumerate serial ports: {}", e);
            return None;
        }
    };

    for port_info in ports {
        let name = port_info.port_name;
        #[cfg(target_os = "linux")]
        {
            if !name.contains("ttyUSB") && !name.contains("ttyACM") {
                continue;
            }
        }
        info!("ccTalk: probing {}...", name);
        if try_cctalk_ping(&name).await {
            info!("ccTalk: device found on {}", name);
            return Some(name);
        }
    }

    warn!("ccTalk: no ccTalk device found on any serial port");
    None
}

/// Opens `port_name` and sends a ccTalk SimplePoll to the coin-acceptor
/// address.  Returns `true` if a valid reply arrives within 300 ms.
async fn try_cctalk_ping(port_name: &str) -> bool {
    let (transport_tx, transport_rx) = tokio_mpsc::channel(4);

    let transport = CcTalkSerialTransport::new(
        transport_rx,
        port_name.to_string(),
        Duration::from_millis(300),
    );

    let transport_task = tokio::spawn(async move {
        let _ = transport.run().await;
    });

    let address = match Category::CoinAcceptor.default_address() {
        Address::Single(addr) | Address::SingleAndRange(addr, _) => addr,
    };
    let validator = CoinValidator::new(
        Device::new(address, Category::CoinAcceptor, ChecksumType::Crc8),
        transport_tx,
    );

    let ok = validator.simple_poll().await.is_ok();
    transport_task.abort();
    ok
}

// ---------------------------------------------------------------------------
// Coin acceptor logic
// ---------------------------------------------------------------------------

/// Parses the 3-char value field (chars 2..5) of a ccTalk coin ID string
/// into luma (sub-units, where 100 luma = 1 AMD).
///
/// The K-suffix encoding places the K *between* significant digits:
///   "5K0" → 5*1000 + 0*100 = 5000 luma
///   "50K" → 50*1000 + 0    = 50000 luma
///   "100" → 100 luma (no K)
///   "000" → None (empty slot)
fn parse_coin_value_luma(value_str: &str) -> Option<usize> {
    if value_str == "000" {
        return None;
    }
    if let Some(k) = value_str.find('K') {
        let before: usize = value_str[..k].parse().ok()?;
        let after: usize = value_str[k + 1..].parse().unwrap_or(0);
        Some(before * 1000 + after * 100)
    } else {
        value_str.parse().ok()
    }
}

/// Returns the face value in AMD from a 6-char ccTalk coin ID string.
/// Returns `None` for empty slots (`"000"` value field) or parse errors.
fn parse_coin_id_amd(id: &str) -> Option<i32> {
    if id.len() < 5 {
        return None;
    }
    let minor = parse_coin_value_luma(&id[2..5])?;
    Some((minor / 100) as i32)
}

/// Runs one connection session: opens the serial port, initialises the
/// validator, and polls until the connection is lost or the event channel
/// is closed.
///
/// Returns `Ok(())` when the upstream event channel is closed (clean shutdown).
/// Returns `Err` when the connection is lost and a reconnect should be attempted.
///
/// `state` is both read (to restore state after a reconnect) and written
/// (to track the latest global and per-slot inhibit state for the next session).
async fn run_session(
    serial_port: &str,
    event_tx: Sender<CoinAcceptorEvent>,
    cmd_rx: &Receiver<CoinAcceptorCommand>,
    state: &mut ControlState,
    coin_overrides: &[[i32; 2]],
    ping_solenoids: bool,
    health: &DeviceHealth,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_path = Path::new(DEFAULT_COIN_SLOT_STATE_PATH);
    let (transport_tx, transport_rx) = tokio_mpsc::channel(32);

    let transport = CcTalkSerialTransport::new(
        transport_rx,
        serial_port.to_string(),
        Duration::from_millis(500),
    );
    // Surface the actual serial open error in health, rather than a subsequent
    // channel-closed error from the transport task.
    let port = transport.open_port()?;

    tokio::spawn(async move {
        if let Err(e) = transport.run_on(port).await {
            error!("ccTalk transport error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let address = match Category::CoinAcceptor.default_address() {
        Address::Single(addr) | Address::SingleAndRange(addr, _) => addr,
    };
    let validator = CoinValidator::new(
        Device::new(address, Category::CoinAcceptor, ChecksumType::Crc8),
        transport_tx,
    );

    validator
        .reset_device()
        .await
        .inspect_err(|e| log::error!("Couldn't reset: {e}"))
        .ok();
    tokio::time::sleep(Duration::from_millis(250)).await;

    info!("Connecting to ccTalk coin validator on {}...", serial_port);
    validator.simple_poll().await?;
    info!("ccTalk coin validator connected");

    let manufacturer = validator.get_manufacturer_id().await?;
    let product = validator.get_product_code().await?;
    let serial = validator.get_serial_number().await?;
    info!(
        "ccTalk device: {} {} (S/N: {})",
        manufacturer, product, serial
    );

    // Build position → AMD value map by fetching the raw coin ID strings and
    // applying the same algorithm as the reference implementation:
    //
    //   id[2..5] is the 3-char value field, e.g.:
    //     "AM5K0A" → "5K0" → 5*1000 + 0*100 = 5000 luma → 50 AMD
    //     "AM50KA" → "50K" → 50*1000 + 0     = 50000 luma → 500 AMD
    //     "AM100A" → "100" → 100 luma → 1 AMD  (direct parse)
    //
    // The library loses the K digit position by extracting all digits first,
    // making "5K0" and "50K" indistinguishable — so we bypass it entirely.
    let mut coin_values: HashMap<u8, i32> = HashMap::new();
    let mut coin_ids: HashMap<u8, String> = HashMap::new();
    for pos in 1u8..=16 {
        let pkt = match validator.send_command(RequestCoinIdCommand::new(pos)).await {
            Ok(p) => p,
            Err(_) => break,
        };
        let data = match pkt.get_data() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if data.len() < 6 {
            continue;
        }
        let id = match std::str::from_utf8(&data[..6]) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if &id[..2] == ".." {
            continue; // unsupported slot
        }
        coin_ids.insert(pos, id.to_string());
        match parse_coin_id_amd(id) {
            Some(amd_value) => {
                info!("ccTalk coin pos={}: id={:?} → {} AMD", pos, id, amd_value);
                coin_values.insert(pos, amd_value);
            }
            None => {
                info!(
                    "ccTalk coin pos={}: id={:?} → empty/unparseable, skipping",
                    pos, id
                );
            }
        }
    }

    // Apply config overrides — these win over the device's coin ID strings.
    for entry in coin_overrides {
        let Ok(pos) = u8::try_from(entry[0]) else {
            warn!(
                "Ignoring invalid ccTalk coin override position {}",
                entry[0]
            );
            continue;
        };
        if !(1..=16).contains(&pos) {
            warn!(
                "Ignoring invalid ccTalk coin override position {}",
                entry[0]
            );
            continue;
        }
        let value = entry[1];
        let prev = coin_values.insert(pos, value);
        info!(
            "ccTalk coin pos={}: override → {} AMD (was {:?})",
            pos, value, prev
        );
    }

    // Commands may have arrived during discovery/initialization, including a
    // Disable from the technical-issue page or a diagnostics slot change. Apply
    // the latest intent before either inhibit register is written.
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            CoinAcceptorCommand::Enable => state.accepting = true,
            CoinAcceptorCommand::Disable => state.accepting = false,
            CoinAcceptorCommand::SetSlotEnabled { position, enabled } => {
                update_slot_intent(state, position, enabled, state_path, &event_tx);
            }
            CoinAcceptorCommand::Reenumerate => {
                health.unavailable("Re-enumerating coin acceptor");
                reenumerate_usb().await;
                return Err("reenumerate".into());
            }
        }
    }

    // The application owns the complete individual mask. Reapply it after every
    // reset/reconnect before restoring the separate master inhibit state.
    validator.set_coin_inhibits(state.coin_inhibits).await?;

    // Start with or restore the desired master inhibit state.
    if state.accepting {
        info!("ccTalk coin acceptor re-enabling after reconnect...");
        validator.disable_master_inhibit().await?;
        info!("ccTalk coin acceptor enabled");
    } else {
        validator.enable_master_inhibit().await?;
        info!("ccTalk coin acceptor initialised, waiting for enable command...");
    }

    let device_label = format!("{} {}", manufacturer, product);
    let _ = event_tx.send(CoinAcceptorEvent::Status(
        format!(
            "{} · {}",
            device_label,
            if state.accepting {
                "Enabled"
            } else {
                "Disabled"
            }
        ),
        1,
    ));
    let _ = event_tx.send(CoinAcceptorEvent::Slots(coin_slot_snapshot(
        &coin_values,
        &coin_ids,
        &state.coin_inhibits,
    )));

    // Click solenoids as confirmation after a re-enumeration reconnect.
    if ping_solenoids {
        info!("ccTalk: clicking solenoids as re-enumeration confirmation");
        if let Err(e) = validator
            .send_command(cc_talk_host::device::device_commands::TestSolenoidsCommand::new(1))
            .await
        {
            warn!("ccTalk: solenoid ping failed: {}", e);
        }
    }

    let delay = validator
        .get_polling_priority()
        .await?
        .as_duration()
        .unwrap_or(Duration::from_millis(100));

    let mut last_counter = 0u8;
    let mut consecutive_errors: u32 = 0;

    loop {
        // Process pending global acceptance and individual slot commands.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                CoinAcceptorCommand::Enable if !state.accepting => {
                    state.accepting = true;
                    // Click the solenoid as an invitation to use the coin machine
                    if let Err(e) = validator
                        .send_command(
                            cc_talk_host::device::device_commands::TestSolenoidsCommand::new(1),
                        )
                        .await
                    {
                        error!("Failed to test solenoids: {}", e);
                    }

                    info!("Enabling ccTalk coin acceptor...");
                    validator.disable_master_inhibit().await?;
                    info!("ccTalk coin acceptor enabled");
                    let _ = event_tx.send(CoinAcceptorEvent::Status(
                        format!("{} · Enabled", device_label),
                        1,
                    ));
                }
                CoinAcceptorCommand::Disable if state.accepting => {
                    state.accepting = false;
                    info!("Disabling ccTalk coin acceptor...");
                    validator.enable_master_inhibit().await?;
                    info!("ccTalk coin acceptor disabled");
                    let _ = event_tx.send(CoinAcceptorEvent::Status(
                        format!("{} · Disabled", device_label),
                        1,
                    ));
                }
                CoinAcceptorCommand::SetSlotEnabled { position, enabled } => {
                    if update_slot_intent(state, position, enabled, state_path, &event_tx) {
                        validator.set_coin_inhibits(state.coin_inhibits).await?;
                        let _ = event_tx.send(CoinAcceptorEvent::Slots(coin_slot_snapshot(
                            &coin_values,
                            &coin_ids,
                            &state.coin_inhibits,
                        )));
                    }
                }
                CoinAcceptorCommand::Reenumerate => {
                    health.unavailable("Re-enumerating coin acceptor");
                    info!("ccTalk: re-enumeration requested via diagnostics");
                    let _ = event_tx.send(CoinAcceptorEvent::Status(
                        "Re-enumerating USB...".to_string(),
                        0,
                    ));
                    reenumerate_usb().await;
                    return Err("reenumerate".into());
                }
                _ => {}
            }
        }

        match validator.poll().await {
            Ok(poll) => {
                health.ready();
                consecutive_errors = 0;

                if poll.event_counter == last_counter {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                last_counter = poll.event_counter;

                if poll.lost_events > 0 {
                    warn!("ccTalk lost {} events", poll.lost_events);
                }

                let mut had_credit = false;
                for event in poll.events {
                    match event {
                        CoinEvent::Credit(credit) => {
                            let value = coin_values.get(&credit.credit).copied().unwrap_or(0);
                            if event_tx.send(CoinAcceptorEvent::Accepted(value)).is_err() {
                                return Ok(());
                            }
                            had_credit = true;
                        }
                        CoinEvent::Error(e) => {
                            let _ = event_tx
                                .send(CoinAcceptorEvent::Error(e.description().to_string()));
                        }
                        CoinEvent::Reset => {
                            info!("ccTalk coin validator reset detected");
                        }
                    }
                }

                // After a credit the coin mechanism (solenoid / motor) is still
                // active for a short time and can inject electrical noise into
                // the serial line.  Pause before the next poll so the line is
                // quiet and the drain_input buffer flush in the transport has
                // time to clear any stray bytes.
                if had_credit {
                    tokio::time::sleep(POST_CREDIT_DELAY).await;
                }
            }
            Err(e) => {
                health.unavailable(format!("Poll failed: {e}"));
                consecutive_errors += 1;
                error!(
                    "ccTalk poll error ({}/{}): {}",
                    consecutive_errors, MAX_CONSECUTIVE_ERRORS, e
                );
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    return Err(format!(
                        "connection lost after {} consecutive errors: {}",
                        consecutive_errors, e
                    )
                    .into());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }

        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "dramma-cctalk-{name}-{:?}-{}",
            std::thread::current().id(),
            std::process::id()
        ))
    }

    #[test]
    fn slot_state_is_one_based_and_independent_of_master_acceptance() {
        let mut state = ControlState {
            accepting: false,
            coin_inhibits: [false; 16],
        };

        state.set_slot_enabled(1, false).unwrap();
        state.set_slot_enabled(16, false).unwrap();
        assert!(state.coin_inhibits[0]);
        assert!(state.coin_inhibits[15]);

        state.accepting = true;
        state.accepting = false;
        assert!(state.coin_inhibits[0]);
        assert!(state.coin_inhibits[15]);

        state.set_slot_enabled(1, true).unwrap();
        assert!(!state.coin_inhibits[0]);
        assert!(state.set_slot_enabled(0, true).is_err());
        assert!(state.set_slot_enabled(17, true).is_err());
    }

    #[test]
    fn disabled_slots_persist_and_invalid_lines_are_ignored() {
        let path = state_path("slot-state");
        let temporary = path.with_extension("tmp");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&temporary);

        assert_eq!(load_coin_inhibits(&path), [false; 16]);

        let mut inhibits = [false; 16];
        inhibits[0] = true;
        inhibits[5] = true;
        inhibits[15] = true;
        save_coin_inhibits(&path, &inhibits).unwrap();
        assert_eq!(load_coin_inhibits(&path), inhibits);

        std::fs::write(&path, "1\n6\n16\n0\n17\ninvalid\n6\n").unwrap();
        assert_eq!(load_coin_inhibits(&path), inhibits);

        std::fs::remove_file(path).unwrap();
        let _ = std::fs::remove_file(temporary);
    }

    #[test]
    fn slot_snapshot_is_sorted_and_contains_only_programmed_positions() {
        let coin_values = HashMap::from([(6, 10), (1, 50)]);
        let coin_ids = HashMap::from([
            (1, "AM5K0A".to_string()),
            (6, "AM1K0A".to_string()),
            (7, "TM000A".to_string()),
        ]);
        let mut inhibits = [false; 16];
        inhibits[5] = true;

        assert_eq!(
            coin_slot_snapshot(&coin_values, &coin_ids, &inhibits),
            vec![
                CoinSlotInfo {
                    position: 1,
                    coin_id: "AM5K0A".to_string(),
                    value: 50,
                    enabled: true,
                },
                CoinSlotInfo {
                    position: 6,
                    coin_id: "AM1K0A".to_string(),
                    value: 10,
                    enabled: false,
                },
            ]
        );
    }
}
