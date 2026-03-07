use std::io::Cursor;
use std::sync::OnceLock;
use std::sync::mpsc::{self, SyncSender};
use std::thread;

use rodio::{Decoder, DeviceSinkBuilder, Player};

const YIPPEE_WAV: &[u8] = include_bytes!("../ui/assets/yippee.wav");

static AUDIO_TX: OnceLock<SyncSender<()>> = OnceLock::new();

/// Initializes the audio subsystem. Must be called once at startup before `play_yippee`.
pub fn init() {
    AUDIO_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<()>(8);

        thread::spawn(move || {
            let handle = match DeviceSinkBuilder::open_default_sink() {
                Ok(h) => h,
                Err(e) => {
                    log::error!("Failed to open audio output: {}", e);
                    return;
                }
            };

            let mixer = handle.mixer();

            while rx.recv().is_ok() {
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
            }
        });

        tx
    });
}

/// Plays 5 yippee sounds simultaneously. Requires `init()` to have been called at startup.
pub fn play_yippee() {
    match AUDIO_TX.get() {
        Some(tx) => {
            if let Err(e) = tx.try_send(()) {
                log::warn!("Audio busy, skipping yippee: {}", e);
            }
        }
        None => log::error!("Audio not initialized — call sound::init() at startup"),
    }
}


