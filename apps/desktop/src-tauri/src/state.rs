use chrono::{DateTime, Local};
use rusqlite::Connection;
use std::{path::Path, sync::Mutex};

use crate::{
    models::{AccountStatus, DailyPoint, RateWindow},
    storage,
};

#[derive(Debug, Clone)]
pub(crate) struct AccountCache {
    pub(crate) cache_key: String,
    pub(crate) status: AccountStatus,
    pub(crate) fetched_at: DateTime<Local>,
}

pub(crate) struct RateCache {
    pub(crate) windows: Vec<RateWindow>,
    pub(crate) fetched_at: DateTime<Local>,
}

#[derive(Default)]
pub(crate) struct RateRetry {
    pub(crate) last_attempt: Option<DateTime<Local>>,
    pub(crate) consecutive_failures: u32,
}

pub(crate) struct RateState {
    pub(crate) cache: Option<RateCache>,
    pub(crate) retry: RateRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshMode {
    Cached,
    Force,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountUsageCache {
    pub(crate) daily: Vec<DailyPoint>,
    pub(crate) fetched_at: DateTime<Local>,
}

pub struct Store {
    pub db: Mutex<Connection>,
    pub expanded: Mutex<bool>,
    pub click_through: Mutex<bool>,
    pub(crate) user_hidden: Mutex<bool>,
    pub(crate) rate_state: Mutex<RateState>,
    pub(crate) rate_refresh: Mutex<()>,
    pub(crate) account_cache: Mutex<Option<AccountCache>>,
    pub(crate) account_refresh: Mutex<()>,
    pub(crate) account_usage_cache: Mutex<Option<AccountUsageCache>>,
    pub(crate) account_usage_refresh: Mutex<()>,
}

impl Store {
    pub fn new(path: &Path) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        storage::initialize(&db)?;
        let expanded = storage::load_window_state(&db)
            .map(|(_, _, expanded)| expanded)
            .unwrap_or(false);
        let rate_cache = storage::load_rate_cache(&db).map(|(windows, fetched_at)| RateCache {
            windows,
            fetched_at,
        });
        Ok(Self {
            db: Mutex::new(db),
            expanded: Mutex::new(expanded),
            click_through: Mutex::new(false),
            user_hidden: Mutex::new(false),
            rate_state: Mutex::new(RateState {
                cache: rate_cache,
                retry: RateRetry::default(),
            }),
            rate_refresh: Mutex::new(()),
            account_cache: Mutex::new(None),
            account_refresh: Mutex::new(()),
            account_usage_cache: Mutex::new(None),
            account_usage_refresh: Mutex::new(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::storage;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    #[test]
    fn store_restores_expanded_flag_from_window_state() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        storage::initialize(&connection).unwrap();
        storage::save_window_expanded(&connection, true).unwrap();
        drop(connection);

        let store = Store::new(file.path()).unwrap();
        assert!(*store.expanded.lock().unwrap());
    }
}
