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
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

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

/// Number of consecutive poll errors before the connection is declared lost
/// and a reconnect is attempted.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Extra pause after a coin credit event, giving the coin mechanism time to
/// settle before the next poll.  Without this the next serial read can catch
/// electrical noise from the solenoid/motor and trigger a framing error.
const POST_CREDIT_DELAY: Duration = Duration::from_millis(800);

#[derive(Debug, Clone)]
pub enum CoinAcceptorCommand {
    Enable,
    Disable,
}

#[derive(Debug, Clone)]
pub enum CoinAcceptorEvent {
    /// Coin accepted; value in AMD (smallest unit).
    Accepted(i32),
    Error(String),
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
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!(
                "Failed to create tokio runtime for ccTalk coin acceptor: {}",
                e
            );
            return;
        }
    };

    rt.block_on(async move {
        let mut enabled = false;

        loop {
            info!("ccTalk: connecting to {}...", serial_port);
            match run_session(
                &serial_port,
                event_tx.clone(),
                &cmd_rx,
                &mut enabled,
                &coin_overrides,
            )
            .await
            {
                Ok(()) => {
                    info!("ccTalk: session ended cleanly, exiting");
                    break;
                }
                Err(e) => {
                    error!(
                        "ccTalk: connection lost ({}), reconnecting in {:?}",
                        e, RECONNECT_DELAY
                    );
                    // Drain any queued commands so we capture the latest
                    // enable/disable intent before sleeping.
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        match cmd {
                            CoinAcceptorCommand::Enable => enabled = true,
                            CoinAcceptorCommand::Disable => enabled = false,
                        }
                    }
                    tokio::time::sleep(RECONNECT_DELAY).await;
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

    async fn run(mut self) -> Result<(), Box<dyn std::error::Error>> {
        let builder = tokio_serial::new(&self.serial_port, CCTALK_BAUD)
            .data_bits(tokio_serial::DataBits::Eight)
            .stop_bits(tokio_serial::StopBits::One)
            .parity(tokio_serial::Parity::None)
            .timeout(self.rw_timeout);

        let mut port = SerialStream::open(&builder)
            .map_err(|e| format!("Failed to open serial port {}: {}", self.serial_port, e))?;

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
/// `enabled` is both read (to restore state after a reconnect) and written
/// (to track the latest inhibit state for the next session).
async fn run_session(
    serial_port: &str,
    event_tx: Sender<CoinAcceptorEvent>,
    cmd_rx: &Receiver<CoinAcceptorCommand>,
    enabled: &mut bool,
    coin_overrides: &[[i32; 2]],
) -> Result<(), Box<dyn std::error::Error>> {
    let (transport_tx, transport_rx) = tokio_mpsc::channel(32);

    let transport = CcTalkSerialTransport::new(
        transport_rx,
        serial_port.to_string(),
        Duration::from_millis(500),
    );

    tokio::spawn(async move {
        if let Err(e) = transport.run().await {
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
        let pos = entry[0] as u8;
        let value = entry[1];
        let prev = coin_values.insert(pos, value);
        info!(
            "ccTalk coin pos={}: override → {} AMD (was {:?})",
            pos, value, prev
        );
    }

    // Clear any individual coin inhibits the device may have persisted so they
    // don't silently block coins independently of the master inhibit.
    if let Err(e) = validator.set_all_coin_inhibits(false).await {
        warn!("Could not clear individual coin inhibits: {}", e);
    }

    // Start with or restore the desired inhibit state.
    if *enabled {
        info!("ccTalk coin acceptor re-enabling after reconnect...");
        validator.disable_master_inhibit().await?;
        info!("ccTalk coin acceptor enabled");
    } else {
        validator.enable_master_inhibit().await?;
        info!("ccTalk coin acceptor initialised, waiting for enable command...");
    }

    let delay = validator
        .get_polling_priority()
        .await?
        .as_duration()
        .unwrap_or(Duration::from_millis(100));

    let mut last_counter = 0u8;
    let mut consecutive_errors: u32 = 0;

    loop {
        // Process any pending Enable / Disable commands.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                CoinAcceptorCommand::Enable if !*enabled => {
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
                    if let Err(e) = validator.disable_master_inhibit().await {
                        error!("Failed to disable master inhibit: {}", e);
                    } else {
                        *enabled = true;
                        info!("ccTalk coin acceptor enabled");
                    }
                }
                CoinAcceptorCommand::Disable if *enabled => {
                    info!("Disabling ccTalk coin acceptor...");
                    if let Err(e) = validator.enable_master_inhibit().await {
                        error!("Failed to enable master inhibit: {}", e);
                    } else {
                        *enabled = false;
                        info!("ccTalk coin acceptor disabled");
                    }
                }
                _ => {}
            }
        }

        match validator.poll().await {
            Ok(poll) => {
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
