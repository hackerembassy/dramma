use image::{DynamicImage, GenericImageView};
use log::{error, info};
use serialport::SerialPort;
use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

pub enum PrinterCommand {
    PrintRaw(Vec<u8>),
    PrintTestReceipt,
    PrintDonationReceipt {
        amount: i32,
        currency: String,
        fund_name: String,
    },
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

/// Helper builder for composing ESC/POS receipt byte buffers (text, graphics, cut commands)
pub struct ReceiptBuilder {
    buf: Vec<u8>,
}

impl Default for ReceiptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptBuilder {
    pub fn new() -> Self {
        let mut builder = Self { buf: Vec::new() };
        builder.init();
        builder
    }

    /// Reset / Initialize printer
    pub fn init(&mut self) -> &mut Self {
        self.buf.extend_from_slice(b"\x1B\x40");
        self
    }

    /// Set alignment: 0 = Left, 1 = Center, 2 = Right
    pub fn align(&mut self, align: u8) -> &mut Self {
        self.buf.extend_from_slice(&[0x1B, 0x61, align.min(2)]);
        self
    }

    pub fn center_align(&mut self) -> &mut Self {
        self.align(1)
    }

    pub fn left_align(&mut self) -> &mut Self {
        self.align(0)
    }

    /// Set text size multiplier (1..=8 for width & height)
    pub fn text_size(&mut self, width_mult: u8, height_mult: u8) -> &mut Self {
        let w = (width_mult.clamp(1, 8) - 1) << 4;
        let h = height_mult.clamp(1, 8) - 1;
        self.buf.extend_from_slice(&[0x1D, 0x21, w | h]);
        self
    }

    /// Reset text size to normal (1x1)
    pub fn normal_size(&mut self) -> &mut Self {
        self.text_size(1, 1)
    }

    /// Append plain text string
    pub fn add_text(&mut self, text: &str) -> &mut Self {
        self.buf.extend_from_slice(text.as_bytes());
        self
    }

    /// Append a bitmap image using ESC/POS `GS v 0` raster command payload
    pub fn add_image(&mut self, img: &DynamicImage) -> &mut Self {
        // Receipt paper print width is typically 576 dots (80mm)
        let max_width = 576;
        let (orig_w, orig_h) = img.dimensions();

        let target_w = orig_w.min(max_width);
        let target_h = (orig_h as f32 * (target_w as f32 / orig_w as f32)) as u32;

        let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);
        let gray = resized.to_luma8();

        // Each horizontal row byte contains 8 monochrome pixels
        let width_bytes = ((target_w + 7) / 8) as usize;
        let height = target_h as usize;

        // GS v 0 format: 0x1D 0x76 0x30 mode xL xH yL yH
        let mode = 0u8; // Normal density
        let x_l = (width_bytes & 0xFF) as u8;
        let x_h = ((width_bytes >> 8) & 0xFF) as u8;
        let y_l = (height & 0xFF) as u8;
        let y_h = ((height >> 8) & 0xFF) as u8;

        self.buf
            .extend_from_slice(&[0x1D, 0x76, 0x30, mode, x_l, x_h, y_l, y_h]);

        for y in 0..height {
            for x_byte in 0..width_bytes {
                let mut byte_val = 0u8;
                for bit in 0..8 {
                    let x_pixel = x_byte * 8 + bit;
                    if x_pixel < target_w as usize {
                        let pixel = gray.get_pixel(x_pixel as u32, y as u32);
                        // Dark pixel -> 1 (print dot), Light pixel -> 0
                        if pixel[0] < 128 {
                            byte_val |= 1 << (7 - bit);
                        }
                    }
                }
                self.buf.push(byte_val);
            }
        }

        self
    }

    /// Feed lines and execute cut sequence (`GS V 0`)
    pub fn feed_and_cut(&mut self) -> &mut Self {
        self.buf.extend_from_slice(b"\n\n\n\n\x1D\x56\x00");
        self
    }

    /// Consume builder and return full raw ESC/POS byte payload
    pub fn build(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
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
                    let mut builder = ReceiptBuilder::new();
                    builder
                        .center_align()
                        .text_size(2, 2)
                        .add_text("HACKER EMBASSY\n")
                        .normal_size()
                        .add_text("DRAMMA KIOSK\n")
                        .add_text("================================\n")
                        .left_align()
                        .add_text("Status: Printer Online :3\n")
                        .add_text("Mode: USB CDC ACM\n")
                        .center_align()
                        .add_text("--------------------------------\n")
                        .add_text("meow!\n")
                        .feed_and_cut();

                    let payload = builder.build();
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print test receipt: {}", e);
                    }
                }
                PrinterCommand::PrintDonationReceipt {
                    amount,
                    currency,
                    fund_name,
                } => {
                    info!(
                        "Printing donation receipt: {} {} to {}",
                        amount, currency, fund_name
                    );
                    let mut builder = ReceiptBuilder::new();
                    builder
                        .center_align()
                        .text_size(2, 2)
                        .add_text("HACKER EMBASSY\n")
                        .normal_size()
                        .add_text("================================\n")
                        .add_text("THANK YOU FOR YOUR DONATION!\n")
                        .add_text("--------------------------------\n")
                        .left_align()
                        .add_text(&format!("Amount:  {} {}\n", amount, currency))
                        .add_text(&format!("Target:  {}\n", fund_name))
                        .center_align()
                        .add_text("================================\n")
                        .add_text("www.hackerembassy.site\n")
                        .feed_and_cut();

                    let payload = builder.build();
                    if let Err(e) = printer.print_raw(&payload) {
                        error!("Failed to print donation receipt: {}", e);
                    }
                }
            }
        }
    });

    tx
}
