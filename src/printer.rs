use log::{error, info};
use serialport::SerialPort;
use std::io::{Read, Write};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::receipt_render::{self, ReceiptData};

pub enum PrinterCommand {
    #[allow(dead_code)]
    Raw(Vec<u8>),
    TestReceipt,
    Receipt(ReceiptData),
    ReceiptWithCompletion {
        data: ReceiptData,
        deadline: Instant,
        reply: Sender<Result<(), String>>,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedPaperStatus {
    Ready,
    PaperOut,
}

fn decode_queued_paper_status(status: u8) -> Option<QueuedPaperStatus> {
    let valid_near_end = matches!(status & 0x03, 0 | 0x03);
    let valid_paper_end = matches!(status & 0x0c, 0 | 0x0c);
    if status & 0x90 != 0 || !valid_near_end || !valid_paper_end {
        return None;
    }

    Some(if status & 0x0c == 0 {
        QueuedPaperStatus::Ready
    } else {
        QueuedPaperStatus::PaperOut
    })
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

    /// Print a payload and wait until the printer has processed the cut at its end.
    ///
    /// `GS r 1` is deliberately used instead of a real-time status request: the
    /// TP80NB executes it only after all earlier bytes in its receive buffer have
    /// been processed. Appending it after the receipt's cut command therefore
    /// gives callers a completion barrier instead of merely confirming that the
    /// USB driver accepted the bytes.
    pub fn print_raw_and_wait(
        &self,
        data: &[u8],
        deadline: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut port = self.open_port()?;
        port.clear(serialport::ClearBuffer::Input)?;
        port.set_timeout(remaining_time(deadline)?)?;

        port.write_all(data)?;
        port.write_all(b"\x1d\x72\x01")?; // GS r 1: queued paper-sensor status
        port.flush()?;

        loop {
            port.set_timeout(remaining_time(deadline)?)?;
            let mut response = [0u8; 1];
            port.read_exact(&mut response)?;

            // A raster image can contain bytes that resemble a real-time DLE
            // status command. Those replies have bit 4 set; ignore them and
            // wait for the queued GS r paper-status response instead.
            let status = response[0];
            match decode_queued_paper_status(status) {
                Some(QueuedPaperStatus::Ready) => return Ok(()),
                Some(QueuedPaperStatus::PaperOut) => {
                    return Err(std::io::Error::other("receipt printer is out of paper").into());
                }
                None => {}
            }
        }
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

fn remaining_time(deadline: Instant) -> Result<Duration, std::io::Error> {
    match deadline.checked_duration_since(Instant::now()) {
        Some(remaining) if !remaining.is_zero() => Ok(remaining),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out waiting for receipt printer",
        )),
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
                PrinterCommand::ReceiptWithCompletion {
                    data,
                    deadline,
                    reply,
                } => {
                    info!(
                        "Printing receipt for @{} and waiting for completion",
                        data.username
                    );
                    let result = if Instant::now() >= deadline {
                        Err("print request expired before reaching the printer".to_string())
                    } else {
                        let payload = receipt_render::render_receipt_to_escpos(&data);
                        printer
                            .print_raw_and_wait(&payload, deadline)
                            .map_err(|error| error.to_string())
                    };
                    if let Err(error) = &result {
                        error!("Failed to complete receipt print: {}", error);
                    }
                    let _ = reply.send(result);
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

    #[test]
    fn recognizes_queued_completion_status() {
        assert_eq!(
            decode_queued_paper_status(0x00),
            Some(QueuedPaperStatus::Ready)
        );
        assert_eq!(
            decode_queued_paper_status(0x03),
            Some(QueuedPaperStatus::Ready)
        );
        assert_eq!(
            decode_queued_paper_status(0x0c),
            Some(QueuedPaperStatus::PaperOut)
        );
    }

    #[test]
    fn queued_completion_ignores_realtime_status_bytes() {
        for status in [0x12, 0x16, 0x1a, 0x72] {
            assert_eq!(decode_queued_paper_status(status), None);
        }
    }
}
