use log::{debug, info};
use serialport::SerialPort;
use std::io::{Read, Write};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CcTalkError {
    #[error("serial port error: {0}")]
    SerialPort(#[from] serialport::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch: expected {expected}, got {got}")]
    ChecksumMismatch { expected: u8, got: u8 },

    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    None,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
}

impl Revision {
    fn from_char(c: char) -> Self {
        match c.to_ascii_uppercase() {
            'A' => Revision::A,
            'B' => Revision::B,
            'C' => Revision::C,
            'D' => Revision::D,
            'E' => Revision::E,
            'F' => Revision::F,
            'G' => Revision::G,
            _ => Revision::None,
        }
    }

    fn to_char(&self) -> char {
        match self {
            Revision::None => ' ',
            Revision::A => 'A',
            Revision::B => 'B',
            Revision::C => 'C',
            Revision::D => 'D',
            Revision::E => 'E',
            Revision::F => 'F',
            Revision::G => 'G',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coin {
    pub currency: [u8; 2],
    pub value: u32,
    pub revision: Revision,
}

impl Coin {
    pub fn from_code(code: &str) -> Option<Self> {
        if code.len() != 6 {
            return None;
        }

        let currency = [code.as_bytes()[0], code.as_bytes()[1]];
        let value_code = &code[2..5];
        let revision_char = code.as_bytes()[5] as char;

        let mut value = 0u32;
        let mut exponent = 0u32;

        for (i, c) in value_code.chars().enumerate() {
            if c.is_ascii_digit() {
                value = value * 10 + c.to_digit(10).unwrap();
            } else if c == 'K' {
                exponent = 1 + i as u32;
            } else if c == 'M' {
                exponent = 4 + i as u32;
            }
        }

        value *= 10u32.pow(exponent);

        Some(Coin {
            currency,
            value,
            revision: Revision::from_char(revision_char),
        })
    }
}

impl std::fmt::Display for Coin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}-{}-{}",
            self.currency[0] as char,
            self.currency[1] as char,
            self.value,
            self.revision.to_char()
        )
    }
}

pub struct CcTalkBus {
    port: Box<dyn SerialPort>,
}

impl CcTalkBus {
    pub fn new(port_path: &str) -> Result<Self, CcTalkError> {
        let port = serialport::new(port_path, 9600)
            .timeout(Duration::from_millis(200))
            .data_bits(serialport::DataBits::Eight)
            .stop_bits(serialport::StopBits::One)
            .parity(serialport::Parity::None)
            .open()?;

        Ok(CcTalkBus { port })
    }

    pub fn send(
        &mut self,
        dest: u8,
        source: u8,
        header: u8,
        data: &[u8],
    ) -> Result<(), CcTalkError> {
        let mut msg = Vec::with_capacity(5 + data.len());
        msg.push(dest);
        msg.push(data.len() as u8);
        msg.push(source);
        msg.push(header);
        msg.extend_from_slice(data);

        let checksum = Self::calculate_checksum(&msg);
        msg.push(checksum);

        self.port.write_all(&msg)?;
        // ccTalk devices echo back the transmitted message
        let mut echo = vec![0u8; msg.len()];
        self.port.read_exact(&mut echo)?;

        Ok(())
    }

    pub fn receive(&mut self) -> Result<(u8, u8, u8, Vec<u8>), CcTalkError> {
        let mut header_buf = [0u8; 4];
        self.port.read_exact(&mut header_buf)?;

        let dest = header_buf[0];
        let data_len = header_buf[1] as usize;
        let source = header_buf[2];
        let header = header_buf[3];

        let mut data = vec![0u8; data_len];
        self.port.read_exact(&mut data)?;

        let mut checksum_buf = [0u8; 1];
        self.port.read_exact(&mut checksum_buf)?;
        let received_checksum = checksum_buf[0];

        let mut full_msg = Vec::with_capacity(4 + data_len);
        full_msg.extend_from_slice(&header_buf);
        full_msg.extend_from_slice(&data);

        let expected_checksum = Self::calculate_checksum(&full_msg);

        if received_checksum != expected_checksum {
            return Err(CcTalkError::ChecksumMismatch {
                expected: expected_checksum,
                got: received_checksum,
            });
        }
        Ok((dest, source, header, data))
    }

    fn calculate_checksum(data: &[u8]) -> u8 {
        let sum: u8 = data.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        0u8.wrapping_sub(sum)
    }
}

pub struct CoinAcceptor {
    bus: CcTalkBus,
    address: u8,
    source_address: u8,
    last_counter: u8,
    supported_coins: Vec<Coin>,
}

impl CoinAcceptor {
    pub fn new(port_path: &str, address: u8) -> Result<Self, CcTalkError> {
        let bus = CcTalkBus::new(port_path)?;
        let source_address = 1;

        let mut acceptor = CoinAcceptor {
            bus,
            address,
            source_address,
            last_counter: 0,
            supported_coins: Vec::new(),
        };

        acceptor.initialize()?;

        Ok(acceptor)
    }

    fn initialize(&mut self) -> Result<(), CcTalkError> {
        // Simple Poll
        self.bus.send(self.address, self.source_address, 254, &[])?;
        let _ = self.bus.receive()?;

        // Request Equipment Category ID
        self.bus.send(self.address, self.source_address, 245, &[])?;
        let (_dest, _src, _hdr, data) = self.bus.receive()?;
        let category = String::from_utf8_lossy(&data);
        if category != "Coin Acceptor" {
            return Err(CcTalkError::InvalidResponse(format!(
                "Invalid category: {}",
                category
            )));
        }

        // Initialize supported coins
        self.supported_coins.clear();
        for i in 1..=16 {
            self.bus
                .send(self.address, self.source_address, 184, &[i as u8])?;
            let (_, _, _, coin_data) = self.bus.receive()?;
            let coin_code = String::from_utf8_lossy(&coin_data);
            if let Some(coin) = Coin::from_code(&coin_code) {
                debug!("Added coin: {} with id {}", coin, i);
                self.supported_coins.push(coin);
            } else {
                break;
            }
        }

        // Read buffered credit to initialize counter
        self.bus.send(self.address, self.source_address, 229, &[])?;
        let (_, _, _, poll_data) = self.bus.receive()?;
        if !poll_data.is_empty() {
            self.last_counter = poll_data[0];
        }

        // enable all coins
        let mut inhibit_mask = vec![0xFFu8; (self.supported_coins.len().div_ceil(8))];
        if inhibit_mask.len() < 2 {
            inhibit_mask.resize(2, 0xFF);
        }
        self.bus
            .send(self.address, self.source_address, 231, &inhibit_mask)?;
        let _ = self.bus.receive()?;

        // enable coin acceptor
        self.bus
            .send(self.address, self.source_address, 228, &[1])?;
        let _ = self.bus.receive()?;

        info!(
            "CcTalk Coin Acceptor initialized at address {}",
            self.address
        );
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<Coin>, CcTalkError> {
        self.bus.send(self.address, self.source_address, 229, &[])?;
        let (_, _, _, data) = self.bus.receive()?;

        if data.is_empty() {
            return Ok(None);
        }

        let counter = data[0];
        if counter != self.last_counter {
            let mut result = None;
            let diff = counter.wrapping_sub(self.last_counter) as usize;
            for i in 0..diff {
                if i >= 5 {
                    break;
                }
                let base = 1 + i * 2;
                if base + 1 >= data.len() {
                    break;
                }

                let coin_id = data[base];
                let error_code = data[base + 1];

                if error_code == 0 && coin_id > 0 && coin_id <= self.supported_coins.len() as u8 {
                    result = Some(self.supported_coins[coin_id as usize - 1].clone());
                }
            }
            self.last_counter = counter;
            return Ok(result);
        }

        Ok(None)
    }
}
