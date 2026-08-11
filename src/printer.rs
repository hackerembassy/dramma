use log::{error, info};
use serialport::SerialPort;
use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use crate::receipt_render::{self, ReceiptData};

pub enum PrinterCommand {
    PrintRaw(Vec<u8>),
    PrintTestReceipt,
    PrintReceipt(ReceiptData),
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
}

/// Spawn background worker thread for printer operations
pub fn init(port_name: String) -> Sender<PrinterCommand> {
    let (tx, rx) = channel::<PrinterCommand>();

    thread::spawn(move || {
        let printer = Printer::new(port_name);
        info!("Receipt printer worker thread initialized");

        while let Ok(cmd) = rx.recv() {
            match cmd {
                PrinterCommand::PrintRaw(payload) => {
                    info!("Printing raw byte payload ({} bytes)", payload.len());
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Printer raw error: {}", e);
                    }
                }
                PrinterCommand::PrintTestReceipt => {
                    info!("Printing test receipt");
                    let data = ReceiptData::new_test_print();
                    let payload = receipt_render::render_receipt_to_escpos(&data);
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print test receipt: {}", e);
                    }
                }
                PrinterCommand::PrintReceipt(data) => {
                    info!("Printing pre-rendered receipt for @{}", data.username);
                    let payload = receipt_render::render_receipt_to_escpos(&data);
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print receipt: {}", e);
                    }
                }
            }
        }
    });

    tx
}
