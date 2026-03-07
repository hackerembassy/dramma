use std::thread;

const YIPPEE_WAV: &[u8] = include_bytes!("../ui/assets/yippee.wav");

fn spawn_yippee() {
    thread::spawn(|| {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = match Command::new("aplay")
            .args(["-q", "-"])
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to spawn aplay: {}", e);
                return;
            }
        };

        if let Some(stdin) = child.stdin.as_mut()
            && let Err(e) = stdin.write_all(YIPPEE_WAV)
        {
            log::error!("Failed to write WAV data to aplay: {}", e);
        }

        if let Err(e) = child.wait() {
            log::error!("aplay exited with error: {}", e);
        }
    });
}

/// Plays 5 yippee sounds simultaneously in parallel background threads.
pub fn play_yippee() {
    for _ in 0..5 {
        spawn_yippee();
    }
}
