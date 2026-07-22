use log::error;
use rusqlite::{Connection, Result as SqlResult, params};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single completed donation, as shown on the donation wall.
#[derive(Debug, Clone)]
pub struct DonationLogEntry {
    pub timestamp: u64,
    pub username: String,
    pub amount: i32,
    pub fund_name: String,
}

fn init_db(db: &Connection) -> SqlResult<()> {
    db.execute(
        "CREATE TABLE IF NOT EXISTS donation_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL,
            username TEXT NOT NULL,
            amount INTEGER NOT NULL,
            fund_name TEXT NOT NULL
        )",
        [],
    )?;
    Ok(())
}

/// Current unix timestamp, shared between a donation's log row and its photo
/// filename (see `camera::photo_filename`) so the two can be re-associated.
pub fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Records a completed donation, running on a dedicated thread so it never
/// blocks the donation flow. Best-effort: a DB hiccup is logged and dropped.
pub fn record(db_path: &str, timestamp: u64, username: &str, amount: i32, fund_name: &str) {
    let db_path = db_path.to_string();
    let username = username.to_string();
    let fund_name = fund_name.to_string();

    thread::spawn(move || {
        let result = (|| -> SqlResult<()> {
            let db = Connection::open(&db_path)?;
            init_db(&db)?;
            db.execute(
                "INSERT INTO donation_log (timestamp, username, amount, fund_name) VALUES (?1, ?2, ?3, ?4)",
                params![timestamp as i64, username, amount, fund_name],
            )?;
            Ok(())
        })();

        if let Err(e) = result {
            error!("Failed to record donation log entry: {}", e);
        }
    });
}

/// Fetches the most recent donations, newest first. Blocking — call off the UI thread.
pub fn fetch_recent(db_path: &str, limit: i64) -> SqlResult<Vec<DonationLogEntry>> {
    let db = Connection::open(db_path)?;
    init_db(&db)?;

    let mut stmt = db.prepare(
        "SELECT timestamp, username, amount, fund_name FROM donation_log ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok(DonationLogEntry {
            timestamp: row.get::<_, i64>(0)? as u64,
            username: row.get(1)?,
            amount: row.get(2)?,
            fund_name: row.get(3)?,
        })
    })?;
    rows.collect()
}
