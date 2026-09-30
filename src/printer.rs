use log::{error, info};
use serialport::SerialPort;
use std::io::{Read, Write};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use crate::receipt_render::{self, ReceiptData};

pub enum PrinterCommand {
    #[allow(dead_code)]
    Raw(Vec<u8>),
    TestReceipt,
    Receipt(ReceiptData),
    Status(Sender<Result<PrinterStatus, String>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrinterStatus {
    pub online: bool,
    pub cover_open: bool,
    pub paper_near_end: bool,
    pub paper_out: bool,
    pub recoverable_error: bool,
    pub cutter_error: bool,
    pub unrecoverable_error: bool,
    pub auto_recoverable_error: bool,
}

impl PrinterStatus {
    fn from_realtime_bytes(status: [u8; 4]) -> Result<Self, std::io::Error> {
        for (index, byte) in status.into_iter().enumerate() {
            // ESC/POS real-time status bytes have bits 0, 1, 4 and 7 fixed
            // to 0, 1, 1 and 0 respectively. Reject unrelated/stale input.
            if byte & 0x93 != 0x12 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid ESC/POS status byte {}: 0x{byte:02X}", index + 1),
                ));
            }
        }

        let [printer, offline, errors, paper] = status;
        Ok(Self {
            online: printer & 0x08 == 0,
            cover_open: offline & 0x04 != 0,
            paper_near_end: paper & 0x0c != 0,
            paper_out: offline & 0x20 != 0 || paper & 0x60 != 0,
            recoverable_error: errors & 0x04 != 0,
            cutter_error: errors & 0x08 != 0,
            unrecoverable_error: errors & 0x20 != 0,
            auto_recoverable_error: errors & 0x40 != 0,
        })
    }
}

pub struct Printer {
    port_name: String,
}

impl Printer {
    pub fn new(port_name: impl Into<String>) -> Self {
        Self {
            port_name: port_name.into(),
        }
    }

    /// Open serial port with DTR/RTS enabled for USB CDC ACM receipt printer
    fn open_port(&self) -> Result<Box<dyn SerialPort>, Box<dyn std::error::Error>> {
        let mut port = serialport::new(&self.port_name, 9600)
            .timeout(Duration::from_secs(3))
            .open()?;

        port.write_data_terminal_ready(true)?;
        port.write_request_to_send(true)?;

        Ok(port)
    }

    /// Send raw ESC/POS byte stream directly to the printer
    pub fn print_raw(&self, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let mut port = self.open_port()?;
        port.write_all(data)?;
        port.flush()?;
        Ok(())
    }

    /// Query the four standard ESC/POS real-time status bytes.
    pub fn status(&self) -> Result<PrinterStatus, Box<dyn std::error::Error>> {
        let mut port = self.open_port()?;
        port.set_timeout(Duration::from_millis(500))?;
        port.clear(serialport::ClearBuffer::Input)?;

        let mut status = [0u8; 4];
        for (index, query) in (1u8..=4).enumerate() {
            port.write_all(&[0x10, 0x04, query])?;
            port.flush()?;
            port.read_exact(&mut status[index..=index])?;
        }

        Ok(PrinterStatus::from_realtime_bytes(status)?)
    }
}

/// Spawn background worker thread for printer operations
pub fn init(port_name: String) -> Sender<PrinterCommand> {
    let (tx, rx) = channel::<PrinterCommand>();

    thread::spawn(move || {
        let printer = Printer::new(port_name);
        info!("Receipt printer worker thread initialized");

        while let Ok(cmd) = rx.recv() {
            match cmd {
                PrinterCommand::Raw(payload) => {
                    info!("Printing raw byte payload ({} bytes)", payload.len());
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Printer raw error: {}", e);
                    }
                }
                PrinterCommand::TestReceipt => {
                    info!("Printing test receipt");
                    let data = ReceiptData::new_test_print();
                    let payload = receipt_render::render_receipt_to_escpos(&data);
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print test receipt: {}", e);
                    }
                }
                PrinterCommand::Receipt(data) => {
                    info!("Printing pre-rendered receipt for @{}", data.username);
                    let payload = receipt_render::render_receipt_to_escpos(&data);
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print receipt: {}", e);
                    }
                }
                PrinterCommand::Status(reply) => {
                    let status = printer.status().map_err(|error| error.to_string());
                    let _ = reply.send(status);
                }
            }
        }
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_ready_printer_status() {
        let status = PrinterStatus::from_realtime_bytes([0x16, 0x12, 0x12, 0x12]).unwrap();

        assert!(status.online);
        assert!(!status.cover_open);
        assert!(!status.paper_near_end);
        assert!(!status.paper_out);
        assert!(!status.recoverable_error);
        assert!(!status.cutter_error);
        assert!(!status.unrecoverable_error);
        assert!(!status.auto_recoverable_error);
    }

    #[test]
    fn decodes_offline_causes_and_errors() {
        let status = PrinterStatus::from_realtime_bytes([0x1a, 0x76, 0x7e, 0x7e]).unwrap();

        assert!(!status.online);
        assert!(status.cover_open);
        assert!(status.paper_near_end);
        assert!(status.paper_out);
        assert!(status.recoverable_error);
        assert!(status.cutter_error);
        assert!(status.unrecoverable_error);
        assert!(status.auto_recoverable_error);
    }

    #[test]
    fn rejects_non_status_input() {
        let error = PrinterStatus::from_realtime_bytes([0x16, 0x12, b'A', 0x12]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
