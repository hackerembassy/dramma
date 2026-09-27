use log::{debug, error, info, warn};
use rusqlite::{Connection, Result as SqlResult};
use serialport::SerialPort;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use thiserror::Error;

// protocol constants
const COMMAND_POLL: &[u8] = &[0x02, 0x03, 0x06, 0x33, 0xDA, 0x81];
const COMMAND_RESET: &[u8] = &[0x02, 0x03, 0x06, 0x30, 0x41, 0xB3];
const COMMAND_ENABLE: &[u8] = &[
    0x02, 0x03, 0x0C, 0x34, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0xB5, 0xC1,
];
const COMMAND_DISABLE: &[u8] = &[
    0x02, 0x03, 0x0C, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x17, 0x0C,
];
const ACK: &[u8] = &[0x02, 0x03, 0x06, 0x00, 0xC2, 0x82];

// status codes
const STATUS_INITIALIZING: u8 = 0x13;
const STATUS_DISABLED: u8 = 0x19;
const STATUS_IDLING: u8 = 0x14;
const STATUS_ACCEPTING: u8 = 0x15;
const STATUS_STACKING: u8 = 0x17;
const STATUS_RETURNING: u8 = 0x18;
const STATUS_HOLDING: u8 = 0x1A;
const STATUS_BUSY: u8 = 0x1B;
const STATUS_STACKER_FULL: u8 = 0x41;
const STATUS_STACKER_REMOVED: u8 = 0x42;
const STATUS_JAM_IN_ACCEPTOR: u8 = 0x43;
const STATUS_JAM_IN_STACKER: u8 = 0x44;
const STATUS_FAILURE: u8 = 0x47;
const STATUS_REJECTED: u8 = 0x1C;
const STATUS_BILL_STACKED: u8 = 0x81;
const STATUS_BILL_RETURNED: u8 = 0x82;

// bill nominals (index-based)
const NOMINAL_1000: u8 = 0x00;
const NOMINAL_5000: u8 = 0x01;
const NOMINAL_10000: u8 = 0x02;
const NOMINAL_2000: u8 = 0x0C;
const NOMINAL_20000: u8 = 0x03;

// reject reasons
const REJECT_INSERTION: u8 = 0x60;
const REJECT_CONVEYING: u8 = 0x64;
const REJECT_IDENTIFICATION: u8 = 0x65;
const REJECT_VERIFICATION: u8 = 0x66;
const REJECT_INHIBITED: u8 = 0x68;
const REJECT_CAPACITY: u8 = 0x69;
const REJECT_OPERATION: u8 = 0x6A;

// failure codes
const FAILURE_55: u8 = 0x55;

#[derive(Debug, Error)]
pub enum CashCodeError {
    #[error("serial port error: {0}")]
    SerialPort(#[from] serialport::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[allow(dead_code)]
    #[error("unexpected ack")]
    UnexpectedAck,

    #[allow(dead_code)]
    #[error("device error: {0}")]
    DeviceError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillNominal {
    Dram1000 = 1000,
    Dram2000 = 2000,
    Dram5000 = 5000,
    Dram10000 = 10000,
    Dram20000 = 20000,
}

impl BillNominal {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            NOMINAL_1000 => Some(BillNominal::Dram1000),
            NOMINAL_2000 => Some(BillNominal::Dram2000),
            NOMINAL_5000 => Some(BillNominal::Dram5000),
            NOMINAL_10000 => Some(BillNominal::Dram10000),
            NOMINAL_20000 => Some(BillNominal::Dram20000),
            _ => None,
        }
    }

    fn value(&self) -> i32 {
        *self as i32
    }
}

#[derive(Debug, Clone)]
pub enum BillEvent {
    Accepted(BillNominal),
    Rejected(String),
    StackerRemoved,
    StackerReplaced,
    Jam(String),
    Error(String),
    /// Lifecycle / device-state update for the diagnostics page.
    /// level: 0 = neutral · 1 = ok · 2 = warn · 3 = error
    Status(String, i32),
}

pub struct CashCode {
    port: Box<dyn SerialPort>,
    stacker_removed: bool,
    availability_error: Option<String>,
    db: Arc<Mutex<Connection>>,
}

/// Read one complete CCNET frame. Silence, truncated frames and electrical
/// noise are failures, not successful empty polls that could keep health green.
fn read_packet(port: &mut impl Read) -> Result<Vec<u8>, CashCodeError> {
    let mut header = [0; 3];
    port.read_exact(&mut header)?;
    if header[0..2] != [0x02, 0x03] || header[2] < 6 {
        return Err(CashCodeError::InvalidResponse(format!(
            "Invalid header: {header:02X?}"
        )));
    }
    let mut packet = vec![0; header[2] as usize];
    packet[..3].copy_from_slice(&header);
    port.read_exact(&mut packet[3..])?;
    let payload_end = packet.len() - 2;
    let expected_crc = u16::from_le_bytes([packet[payload_end], packet[payload_end + 1]]);
    if packet_crc(&packet[..payload_end]) != expected_crc {
        return Err(CashCodeError::InvalidResponse("Checksum mismatch".into()));
    }
    Ok(packet)
}

fn packet_crc(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

impl CashCode {
    pub fn new(port_path: &str, db_path: &str) -> Result<Self, CashCodeError> {
        info!("opening serial port: {}", port_path);

        let port = serialport::new(port_path, 19200)
            .timeout(Duration::from_millis(100))
            .open()?;

        info!("opening database: {}", db_path);
        let db = Connection::open(db_path)?;

        // initialize database
        Self::init_database(&db)?;

        Ok(CashCode {
            port,
            stacker_removed: false,
            availability_error: Some("Initializing bill acceptor".into()),
            db: Arc::new(Mutex::new(db)),
        })
    }

    fn init_database(db: &Connection) -> SqlResult<()> {
        db.execute(
            "CREATE TABLE IF NOT EXISTS accepted_bills (
                nominal INTEGER PRIMARY KEY,
                quantity INTEGER NOT NULL
            )",
            [],
        )?;

        let nominals = [1000, 2000, 5000, 10000, 20000];
        for nominal in nominals {
            db.execute(
                "INSERT OR IGNORE INTO accepted_bills (nominal, quantity) VALUES (?1, 0)",
                [nominal],
            )?;
        }

        Ok(())
    }

    fn send_command(&mut self, command: &[u8]) -> Result<(), CashCodeError> {
        self.port.write_all(command)?;
        thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    fn read_response(&mut self) -> Result<Vec<u8>, CashCodeError> {
        thread::sleep(Duration::from_millis(20));
        read_packet(&mut self.port)
    }

    pub fn availability_error(&self) -> Option<&str> {
        self.availability_error.as_deref()
    }

    fn clear_buffer(&mut self) -> Result<(), CashCodeError> {
        let bytes_available = self.port.bytes_to_read()? as usize;
        if bytes_available > 0 {
            let mut buffer = vec![0u8; bytes_available];
            self.port.read_exact(&mut buffer)?;
        }
        Ok(())
    }

    fn send_ack(&mut self) -> Result<(), CashCodeError> {
        self.port.write_all(ACK)?;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), CashCodeError> {
        info!("resetting bill acceptor...");
        self.send_command(COMMAND_RESET)?;

        let response = self.read_response()?;
        if response == ACK {
            info!("bill acceptor reset ACK");
            self.clear_buffer()?;
        } else {
            warn!("unexpected response to reset: {:02X?}", response);
            self.send_ack()?;
            self.clear_buffer()?;
        }

        Ok(())
    }

    pub fn enable(&mut self) -> Result<(), CashCodeError> {
        info!("enabling bill acceptance...");
        self.send_command(COMMAND_ENABLE)?;

        let response = self.read_response()?;
        if response == ACK {
            info!("bill acceptance enabled");
            self.clear_buffer()?;
        } else {
            warn!("unexpected response to enable: {:02X?}", response);
            self.send_ack()?;
            self.clear_buffer()?;
        }

        Ok(())
    }

    pub fn disable(&mut self) -> Result<(), CashCodeError> {
        info!("disabling bill acceptance...");
        self.send_command(COMMAND_DISABLE)?;

        let response = self.read_response()?;
        if response == ACK {
            info!("bill acceptance disabled");
            self.clear_buffer()?;
        } else {
            warn!("unexpected response to disable: {:02X?}", response);
            self.send_ack()?;
            self.clear_buffer()?;
        }

        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<BillEvent>, CashCodeError> {
        self.send_command(COMMAND_POLL)?;

        let response = self.read_response()?;

        let status = response[3];
        if matches!(
            status,
            STATUS_FAILURE | STATUS_REJECTED | STATUS_BILL_STACKED
        ) && response.len() < 7
        {
            return Err(CashCodeError::InvalidResponse("Missing status data".into()));
        }
        self.availability_error = match status {
            STATUS_INITIALIZING => Some("Bill acceptor is initializing".into()),
            STATUS_STACKER_FULL => Some("Stacker full".into()),
            STATUS_STACKER_REMOVED => Some("Stacker removed".into()),
            STATUS_JAM_IN_ACCEPTOR => Some("Bill jam in acceptor".into()),
            STATUS_JAM_IN_STACKER => Some("Bill jam in stacker".into()),
            STATUS_FAILURE => Some(format!("Device failure 0x{:02X}", response[4])),
            STATUS_DISABLED | STATUS_IDLING | STATUS_ACCEPTING | STATUS_STACKING
            | STATUS_RETURNING | STATUS_HOLDING | STATUS_BUSY | STATUS_BILL_RETURNED
            | STATUS_REJECTED | STATUS_BILL_STACKED => None,
            _ => Some(format!("Unexpected device status 0x{status:02X}")),
        };

        let event = match status {
            STATUS_INITIALIZING => {
                self.send_ack()?;
                info!("bill acceptor initialized");
                self.clear_buffer()?;
                None
            }

            STATUS_DISABLED => {
                self.send_ack()?;
                debug!("bill acceptor is disabled");
                self.clear_buffer()?;

                // check if stacker was recently removed and is now back
                if self.stacker_removed {
                    info!("stacker replaced");
                    self.stacker_removed = false;
                    Some(BillEvent::StackerReplaced)
                } else {
                    None
                }
            }

            STATUS_IDLING | STATUS_ACCEPTING | STATUS_STACKING | STATUS_RETURNING
            | STATUS_HOLDING | STATUS_BUSY | STATUS_BILL_RETURNED => {
                self.send_ack()?;
                self.clear_buffer()?;
                None
            }

            STATUS_STACKER_REMOVED => {
                self.send_ack()?;
                if !self.stacker_removed {
                    self.stacker_removed = true;
                    error!("ERR: stacker removed");
                    self.clear_buffer()?;
                    Some(BillEvent::StackerRemoved)
                } else {
                    self.clear_buffer()?;
                    None
                }
            }

            STATUS_STACKER_FULL => {
                self.send_ack()?;
                self.clear_buffer()?;
                Some(BillEvent::Error("Stacker full".into()))
            }

            STATUS_JAM_IN_STACKER => {
                self.send_ack()?;
                error!("ERR: bill jam in stacker");
                self.clear_buffer()?;
                Some(BillEvent::Jam("Bill jam in stacker".to_string()))
            }

            STATUS_JAM_IN_ACCEPTOR => {
                self.send_ack()?;
                error!("ERR: bill jam in acceptor");
                self.clear_buffer()?;
                Some(BillEvent::Jam("Bill jam in acceptor".to_string()))
            }

            STATUS_FAILURE => {
                let error_code = response[4];
                self.send_ack()?;
                self.clear_buffer()?;

                match error_code {
                    FAILURE_55 => {
                        error!("ERROR: FAILURE 55 (sensor cover opened?)");
                        Some(BillEvent::Error("FAILURE 55".to_string()))
                    }
                    _ => {
                        error!("FAILURE with unknown code: 0x{:02X}", error_code);
                        Some(BillEvent::Error(format!("FAILURE 0x{:02X}", error_code)))
                    }
                }
            }

            STATUS_REJECTED => {
                let reject_code = response[4];
                self.send_ack()?;
                self.clear_buffer()?;

                let reason = match reject_code {
                    REJECT_INSERTION => "Insertion error",
                    REJECT_CONVEYING => "Conveying error",
                    REJECT_IDENTIFICATION => "Identification error",
                    REJECT_VERIFICATION => "Verification error",
                    REJECT_INHIBITED => "Denomination inhibited",
                    REJECT_CAPACITY => "Capacity error",
                    REJECT_OPERATION => "Operation error",
                    _ => "Unknown error",
                };

                warn!("bill rejected: {}", reason);
                Some(BillEvent::Rejected(reason.to_string()))
            }

            STATUS_BILL_STACKED => {
                let nominal_code = response[4];
                self.send_ack()?;
                self.clear_buffer()?;

                if let Some(nominal) = BillNominal::from_code(nominal_code) {
                    info!("bill accepted: {} dram", nominal.value());
                    self.record_bill(nominal)?;
                    Some(BillEvent::Accepted(nominal))
                } else {
                    warn!("bill accepted with unknown nominal: 0x{:02X}", nominal_code);
                    Some(BillEvent::Error(format!(
                        "Unknown nominal: 0x{:02X}",
                        nominal_code
                    )))
                }
            }

            _ => {
                warn!(
                    "Unknown status code: 0x{:02X}, response: {:02X?}",
                    status, response
                );
                None
            }
        };

        Ok(event)
    }

    fn record_bill(&self, nominal: BillNominal) -> Result<(), CashCodeError> {
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE accepted_bills SET quantity = quantity + 1 WHERE nominal = ?1",
            [nominal.value()],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_bill_counts(&self) -> Result<Vec<(i32, i32)>, CashCodeError> {
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT nominal, quantity FROM accepted_bills ORDER BY nominal")?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    pub fn get_total_amount(&self) -> Result<i32, CashCodeError> {
        let db = self.db.lock().unwrap();
        let total: i32 = db
            .query_row(
                "SELECT SUM(nominal * quantity) FROM accepted_bills",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    #[test]
    fn protocol_fixtures_have_valid_checksums() {
        for packet in [
            ACK,
            COMMAND_POLL,
            COMMAND_RESET,
            COMMAND_ENABLE,
            COMMAND_DISABLE,
        ] {
            assert_eq!(read_packet(&mut Cursor::new(packet)).unwrap(), packet);
        }
    }

    #[test]
    fn empty_truncated_corrupted_and_invalid_length_frames_fail() {
        for data in [
            vec![],
            ACK[..4].to_vec(),
            vec![2, 3, 0],
            vec![2, 3, 6, 0, 0, 0],
            vec![0, 0, 6],
        ] {
            assert!(read_packet(&mut Cursor::new(data)).is_err());
        }
    }

    #[test]
    fn reads_fragmented_frames_without_consuming_the_next_frame() {
        struct Fragmented(Cursor<Vec<u8>>);
        impl Read for Fragmented {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = buffer.len().min(1);
                self.0.read(&mut buffer[..count])
            }
        }
        let mut port = Fragmented(Cursor::new([ACK, ACK].concat()));
        assert_eq!(read_packet(&mut port).unwrap(), ACK);
        assert_eq!(read_packet(&mut port).unwrap(), ACK);
    }

    #[test]
    fn serial_timeout_is_not_a_successful_poll() {
        struct Silent;
        impl Read for Silent {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        assert!(
            matches!(read_packet(&mut Silent), Err(CashCodeError::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
        );
    }
}
