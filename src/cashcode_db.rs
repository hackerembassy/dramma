use rusqlite::{Connection, Result as SqlResult};
use std::sync::{Arc, Mutex};

use crate::cashcode::{BillNominal, CashCodeError};

#[derive(Clone)]
pub struct CashCodeDb {
    pub conn: Arc<Mutex<Connection>>,
}

impl CashCodeDb {
    pub fn open(db_path: &str) -> Result<Self, CashCodeError> {
        let conn = Connection::open(db_path)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init(db: &Connection) -> SqlResult<()> {
        db.execute(
            "CREATE TABLE IF NOT EXISTS accepted_bills (
                nominal INTEGER PRIMARY KEY,
                quantity INTEGER NOT NULL
            )",
            [],
        )?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS accepted_coins (
                nominal INTEGER PRIMARY KEY,
                quantity INTEGER NOT NULL
            )",
            [],
        )?;

        Ok(())
    }

    pub fn record_bill(&self, nominal: BillNominal) -> Result<(), CashCodeError> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO accepted_bills (nominal, quantity) VALUES (?1, 1)
             ON CONFLICT(nominal) DO UPDATE SET quantity = quantity + 1",
            [nominal.value()],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_bill_counts(&self) -> Result<Vec<(i32, i32)>, CashCodeError> {
        let db = self.conn.lock().unwrap();
        let mut stmt =
            db.prepare("SELECT nominal, quantity FROM accepted_bills ORDER BY nominal")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<SqlResult<Vec<_>>>().map_err(Into::into)
    }

    pub fn record_coin(&self, value: i32) -> Result<(), CashCodeError> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO accepted_coins (nominal, quantity) VALUES (?1, 1)
             ON CONFLICT(nominal) DO UPDATE SET quantity = quantity + 1",
            [value],
        )?;
        Ok(())
    }

    pub fn get_total_amount(&self) -> Result<i32, CashCodeError> {
        let db = self.conn.lock().unwrap();
        let total: i32 = db
            .query_row(
                "SELECT SUM(nominal * quantity) FROM accepted_bills",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(total)
    }
}
