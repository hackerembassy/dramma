use std::io::Cursor;
use std::thread;

use rodio::{Decoder, DeviceSinkBuilder, Player};

const YIPPEE_WAV: &[u8] = include_bytes!("../ui/assets/yippee.wav");

/// Plays 5 yippee sounds simultaneously through ALSA via rodio.
pub fn play_yippee() {
    thread::spawn(|| {
        let handle = match DeviceSinkBuilder::open_default_sink() {
            Ok(h) => h,
            Err(e) => {
                log::error!("Failed to open audio output: {}", e);
                return;
            }
        };

        let mixer = handle.mixer();

        let players: Vec<Player> = (0..5)
            .filter_map(|_| {
                let source = match Decoder::try_from(Cursor::new(YIPPEE_WAV)) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("Failed to decode WAV: {}", e);
                        return None;
                    }
                };
                let player = Player::connect_new(&mixer);
                player.append(source);
                Some(player)
            })
            .collect();

        if let Some(last) = players.last() {
            last.sleep_until_end();
        }
    });
}


