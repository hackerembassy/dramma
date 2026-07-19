use std::io::Cursor;
use std::sync::OnceLock;
use std::sync::mpsc::{self, SyncSender};
use std::thread;

use rodio::{Decoder, DeviceSinkBuilder, Player};

const YIPPEE_WAV: &[u8] = include_bytes!("../ui/assets/yippee.wav");
const TWO_MINUTES_LEFT_WAV: &[u8] = include_bytes!("../ui/assets/two_minutes_left.wav");
const ONE_MINUTE_LEFT_WAV: &[u8] = include_bytes!("../ui/assets/one_minute_left.wav");

enum SoundEvent {
    Yippee,
    TwoMinutesLeft,
    OneMinuteLeft,
}

static AUDIO_TX: OnceLock<SyncSender<SoundEvent>> = OnceLock::new();

/// Initializes the audio subsystem. Must be called once at startup before any `play_*` calls.
pub fn init() {
    AUDIO_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<SoundEvent>(8);

        thread::spawn(move || {
            let mut sink = None;

            while let Ok(event) = rx.recv() {
                if sink.is_none() {
                    match DeviceSinkBuilder::open_default_sink() {
                        Ok(h) => sink = Some(h),
                        Err(e) => {
                            log::error!("Failed to open audio output: {}", e);
                            continue;
                        }
                    }
                }

                let handle = sink.as_ref().unwrap();

                let wav_bytes: &[u8] = match event {
                    SoundEvent::Yippee => YIPPEE_WAV,
                    SoundEvent::TwoMinutesLeft => TWO_MINUTES_LEFT_WAV,
                    SoundEvent::OneMinuteLeft => ONE_MINUTE_LEFT_WAV,
                };
                let source = match Decoder::try_from(Cursor::new(wav_bytes)) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("Failed to decode WAV: {}", e);
                        continue;
                    }
                };
                let player = Player::connect_new(handle.mixer());
                player.append(source);
                player.sleep_until_end();
            }
        });

        tx
    });
}

fn play_sound(event: SoundEvent) {
    match AUDIO_TX.get() {
        Some(tx) => {
            if let Err(e) = tx.try_send(event) {
                log::warn!("Audio busy, skipping sound: {}", e);
            }
        }
        None => log::error!("Audio not initialized — call sound::init() at startup"),
    }
}

/// Plays the yippee sound.
/// Requires `init()` to have been called at startup.
pub fn play_yippee() {
    play_sound(SoundEvent::Yippee);
}

/// Plays the "2 minutes left" announcement.
pub fn play_two_minutes_left() {
    play_sound(SoundEvent::TwoMinutesLeft);
}

/// Plays the "1 minute left" announcement.
pub fn play_one_minute_left() {
    play_sound(SoundEvent::OneMinuteLeft);
}
