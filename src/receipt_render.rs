use ab_glyph::{FontRef, PxScale};
use chrono::{DateTime, Local};
use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use qrcode::QrCode;

const FONT_REGULAR_BYTES: &[u8] = include_bytes!("../ui/assets/Roboto-Regular.ttf");
const FONT_BOLD_BYTES: &[u8] = include_bytes!("../ui/assets/Roboto-Bold.ttf");
const LOGO_BYTES: &[u8] = include_bytes!("../ui/assets/xkem.png");

const CANVAS_WIDTH: u32 = 576; // 80mm thermal paper @ 203 DPI

#[derive(Debug, Clone)]
pub enum ReceiptKind {
    Donation { fund_name: String, fund_id: i32 },
    ArcadeGame { game_name: String, duration_str: String },
    TestPrint,
}

#[derive(Debug, Clone)]
pub struct ReceiptData {
    pub kind: ReceiptKind,
    pub username: String,
    pub amount: i32,
    pub currency: String,
    pub timestamp: DateTime<Local>,
}

impl ReceiptData {
    pub fn new_test_print() -> Self {
        let now = Local::now();
        Self {
            kind: ReceiptKind::TestPrint,
            username: "diagnostics".to_string(),
            amount: 0,
            currency: "AMD".to_string(),
            timestamp: now,
        }
    }

    pub fn new_donation(
        username: String,
        fund_name: String,
        fund_id: i32,
        amount: i32,
    ) -> Self {
        let now = Local::now();
        Self {
            kind: ReceiptKind::Donation { fund_name, fund_id },
            username,
            amount,
            currency: "AMD".to_string(),
            timestamp: now,
        }
    }

    pub fn new_game(
        username: String,
        game_name: String,
        duration_str: String,
        amount: i32,
    ) -> Self {
        let now = Local::now();
        Self {
            kind: ReceiptKind::ArcadeGame {
                game_name,
                duration_str,
            },
            username,
            amount,
            currency: "AMD".to_string(),
            timestamp: now,
        }
    }
}

/// Pre-renders receipt to a 2D image canvas matching the user's sketch
pub fn render_receipt_canvas(data: &ReceiptData) -> RgbImage {
    let font_regular = FontRef::try_from_slice(FONT_REGULAR_BYTES).expect("Valid regular font");
    let font_bold = FontRef::try_from_slice(FONT_BOLD_BYTES).expect("Valid bold font");

    // Dynamic height calculation
    let canvas_height = match data.kind {
        ReceiptKind::TestPrint => 450,
        _ => 600,
    };

    let mut canvas: RgbImage = ImageBuffer::from_pixel(CANVAS_WIDTH, canvas_height, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);

    let mut y_cursor: i32 = 10;

    // Helper: draw dithered grey line
    let draw_grey_divider = |canvas: &mut RgbImage, y: i32| {
        for x in 10..(CANVAS_WIDTH as i32 - 10) {
            if (x + y) % 2 == 0 {
                if x >= 0 && x < CANVAS_WIDTH as i32 && y >= 0 && y < canvas.height() as i32 {
                    canvas.put_pixel(x as u32, y as u32, black);
                }
            }
        }
    };

    // --- HEADER SECTION ---
    if let Ok(logo_img) = image::load_from_memory(LOGO_BYTES) {
        let logo_resized = logo_img.resize(110, 80, image::imageops::FilterType::Lanczos3);
        let (lw, lh) = logo_resized.dimensions();
        for ly in 0..lh {
            for lx in 0..lw {
                let p = logo_resized.get_pixel(lx, ly);
                // Alpha blend onto white canvas
                if p[3] > 128 {
                    let cx = 15 + lx;
                    let cy = (y_cursor as u32) + ly;
                    if cx < CANVAS_WIDTH && cy < canvas.height() {
                        canvas.put_pixel(cx, cy, Rgb([p[0], p[1], p[2]]));
                    }
                }
            }
        }
    }

    draw_text_mut(&mut canvas, black, 135, y_cursor + 5, PxScale::from(36.0), &font_bold, "Hacker Embassy");
    draw_text_mut(&mut canvas, black, 135, y_cursor + 40, PxScale::from(28.0), &font_regular, "> dramma");

    let time_str = data.timestamp.format("%H:%M:%S").to_string();
    let date_str = data.timestamp.format("%d.%m.%y").to_string();

    let mut time_sub: RgbImage = ImageBuffer::from_pixel(130, 55, Rgb([255, 255, 255]));
    draw_text_mut(&mut time_sub, black, 0, 0, PxScale::from(19.0), &font_bold, &time_str);
    draw_text_mut(&mut time_sub, black, 0, 26, PxScale::from(19.0), &font_bold, &date_str);

    let time_rot = image::imageops::rotate90(&time_sub);
    let (rw, rh) = time_rot.dimensions();
    let rx_start = CANVAS_WIDTH - rw - 15;
    for ry in 0..rh {
        for rx in 0..rw {
            let p = time_rot.get_pixel(rx, ry);
            if p[0] < 200 {
                let cx = rx_start + rx;
                let cy = (y_cursor as u32) + ry;
                if cx < CANVAS_WIDTH && cy < canvas.height() {
                    canvas.put_pixel(cx, cy, *p);
                }
            }
        }
    }

    y_cursor += 90;
    draw_grey_divider(&mut canvas, y_cursor);
    y_cursor += 15;

    // --- TITLE SECTION ---
    let title_text = match data.kind {
        ReceiptKind::TestPrint => ">>           TEST PRINT           <<",
        _ => ">>            RECEIPT            <<",
    };
    draw_text_mut(&mut canvas, black, 75, y_cursor, PxScale::from(26.0), &font_bold, title_text);
    y_cursor += 35;
    draw_grey_divider(&mut canvas, y_cursor);
    y_cursor += 20;

    // --- BODY SECTION ---
    match &data.kind {
        ReceiptKind::Donation { fund_name, fund_id } => {
            let handle_str = if data.username.starts_with('@') {
                data.username.clone()
            } else {
                format!("@{}", data.username)
            };
            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(24.0), &font_bold, &handle_str);
            y_cursor += 36;

            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(22.0), &font_bold, "Donated to");
            y_cursor += 32;

            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(24.0), &font_bold, fund_name);
            let id_str = format!("[ {} ]", fund_id);
            draw_text_mut(&mut canvas, black, CANVAS_WIDTH as i32 - 100, y_cursor, PxScale::from(22.0), &font_bold, &id_str);
            y_cursor += 42;
        }
        ReceiptKind::ArcadeGame { game_name, duration_str } => {
            let handle_str = if data.username.starts_with('@') {
                data.username.clone()
            } else {
                format!("@{}", data.username)
            };
            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(24.0), &font_bold, &handle_str);
            y_cursor += 36;

            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(22.0), &font_regular, "Played in");
            draw_text_mut(&mut canvas, black, CANVAS_WIDTH as i32 - 200, y_cursor, PxScale::from(24.0), &font_bold, game_name);
            y_cursor += 34;

            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(22.0), &font_regular, "For");
            draw_text_mut(&mut canvas, black, CANVAS_WIDTH as i32 - 150, y_cursor, PxScale::from(24.0), &font_bold, duration_str);
            y_cursor += 42;
        }
        ReceiptKind::TestPrint => {
            draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(22.0), &font_regular, "If you can read this,");
            draw_text_mut(&mut canvas, black, CANVAS_WIDTH as i32 - 240, y_cursor + 32, PxScale::from(22.0), &font_bold, "printer works fine.");
            y_cursor += 75;
        }
    }

    draw_grey_divider(&mut canvas, y_cursor);
    y_cursor += 20;

    // --- PAYMENT SECTION ---
    if !matches!(data.kind, ReceiptKind::TestPrint) {
        draw_text_mut(&mut canvas, black, 160, y_cursor, PxScale::from(26.0), &font_bold, ">>      PAID      <<");
        y_cursor += 35;

        draw_text_mut(&mut canvas, black, 20, y_cursor, PxScale::from(24.0), &font_bold, "By cash");
        let amount_str = format!("{} {}", data.amount, data.currency);
        draw_text_mut(&mut canvas, black, CANVAS_WIDTH as i32 - 170, y_cursor, PxScale::from(24.0), &font_bold, &amount_str);
        y_cursor += 42;

        draw_grey_divider(&mut canvas, y_cursor);
        y_cursor += 20;
    }

    // --- FOOTER SECTION ---
    let _qr_size = if let Ok(qr) = QrCode::new("https://hackem.cc") {
        let qr_img = qr.render::<image::Luma<u8>>()
            .quiet_zone(false)
            .min_dimensions(116, 116)
            .build();
        let (qw, qh) = qr_img.dimensions();

        for qy in 0..qh {
            for qx in 0..qw {
                let p = qr_img.get_pixel(qx, qy);
                let color = if p[0] < 128 { black } else { Rgb([255, 255, 255]) };
                let cx = 20 + qx;
                let cy = (y_cursor as u32) + qy;
                if cx < CANVAS_WIDTH && cy < canvas.height() {
                    canvas.put_pixel(cx, cy, color);
                }
            }
        }
        qh
    } else {
        116
    };

    // Bottom Right: Address text
    let text_x = 160;
    draw_text_mut(&mut canvas, black, text_x, y_cursor, PxScale::from(24.0), &font_bold, "Thanks! :3");
    draw_text_mut(&mut canvas, black, text_x, y_cursor + 30, PxScale::from(22.0), &font_regular, "Baghramyan 60");
    draw_text_mut(&mut canvas, black, text_x, y_cursor + 58, PxScale::from(22.0), &font_regular, "Yerevan, Armenia");
    draw_text_mut(&mut canvas, black, text_x, y_cursor + 88, PxScale::from(24.0), &font_bold, "hackem.cc");

    canvas
}

/// Applies Floyd-Steinberg dithering and converts canvas image into ESC/POS raster payload
pub fn render_receipt_to_escpos(data: &ReceiptData) -> Vec<u8> {
    let rgb_canvas = render_receipt_canvas(data);
    let (width, height) = rgb_canvas.dimensions();
    let mut gray_canvas = DynamicImage::ImageRgb8(rgb_canvas).to_luma8();

    // Floyd-Steinberg Dithering to 1-bit monochrome
    for y in 0..height {
        for x in 0..width {
            let old_p = gray_canvas.get_pixel(x, y)[0] as i16;
            let new_p = if old_p < 128 { 0 } else { 255 };
            gray_canvas.put_pixel(x, y, Luma([new_p as u8]));

            let err = old_p - new_p;

            let distribute_err = |canvas: &mut GrayImage, cx: i32, cy: i32, factor: i16| {
                if cx >= 0 && cx < width as i32 && cy >= 0 && cy < height as i32 {
                    let current = canvas.get_pixel(cx as u32, cy as u32)[0] as i16;
                    let updated = (current + (err * factor) / 16).clamp(0, 255);
                    canvas.put_pixel(cx as u32, cy as u32, Luma([updated as u8]));
                }
            };

            distribute_err(&mut gray_canvas, x as i32 + 1, y as i32, 7);
            distribute_err(&mut gray_canvas, x as i32 - 1, y as i32 + 1, 3);
            distribute_err(&mut gray_canvas, x as i32, y as i32 + 1, 5);
            distribute_err(&mut gray_canvas, x as i32 + 1, y as i32 + 1, 1);
        }
    }

    // Convert 1-bit monochrome image into ESC/POS GS v 0 raster command
    let width_bytes = ((width + 7) / 8) as usize;
    let h_usize = height as usize;

    let mut buf = Vec::new();
    // Initialize printer: ESC @
    buf.extend_from_slice(b"\x1B\x40");

    // GS v 0 format: 0x1D 0x76 0x30 mode xL xH yL yH
    let mode = 0u8;
    let x_l = (width_bytes & 0xFF) as u8;
    let x_h = ((width_bytes >> 8) & 0xFF) as u8;
    let y_l = (h_usize & 0xFF) as u8;
    let y_h = ((h_usize >> 8) & 0xFF) as u8;

    buf.extend_from_slice(&[0x1D, 0x76, 0x30, mode, x_l, x_h, y_l, y_h]);

    for y in 0..height {
        for x_byte in 0..width_bytes {
            let mut byte_val = 0u8;
            for bit in 0..8 {
                let x_pixel = x_byte * 8 + bit;
                if x_pixel < width as usize {
                    let pixel = gray_canvas.get_pixel(x_pixel as u32, y);
                    if pixel[0] < 128 {
                        byte_val |= 1 << (7 - bit);
                    }
                }
            }
            buf.push(byte_val);
        }
    }

    // Feed lines & Cut sequence
    buf.extend_from_slice(b"\n\n\n\n\x1D\x56\x00");

    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_test_receipt() {
        let data = ReceiptData::new_test_print();
        let escpos = render_receipt_to_escpos(&data);
        assert!(!escpos.is_empty());
    }
}

