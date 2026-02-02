use std::collections::HashSet;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{DbEntry, DbStatus, GenericDatabase, log_debug, log_error};

const CACHE_FILE: &str = "db_traits.json";
const DB_NAME: &str = "traits";

#[derive(Serialize, Deserialize, Clone)]
pub struct GwTrait {
    pub id: u32,
    pub name: String,
}

impl DbEntry for GwTrait {
    fn id(&self) -> u32 {
        self.id
    }

    fn display_label(&self) -> String {
        format!("[{}] {}", self.id, self.name)
    }

    fn matches_search(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query)
    }
}

pub static DB: Lazy<Mutex<GenericDatabase<GwTrait>>> =
    Lazy::new(|| Mutex::new(GenericDatabase::default()));

pub fn ensure_loaded() {
    {
        let db = DB.lock();
        log_debug(&format!("[{}] ensure_loaded - status: {:?}", DB_NAME, status_str(db.status)));
        if matches!(db.status, DbStatus::Loading | DbStatus::Loaded | DbStatus::Updating) {
            return;
        }
    }
    {
        let mut db = DB.lock();
        db.status = DbStatus::Loading;
    }
    log_debug(&format!("[{}] Spawning load thread", DB_NAME));

    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            if let Some(entries) = super::load_from_cache::<GwTrait>(CACHE_FILE) {
                if !entries.is_empty() {
                    log_debug(&format!("[{}] Loaded {} entries from cache", DB_NAME, entries.len()));
                    let mut db = DB.lock();
                    db.entries = entries;
                    db.status = DbStatus::Loaded;
                    db.progress = None;
                    return;
                }
            }
            fetch_from_api();
        });
        if let Err(e) = result {
            let msg = format!("[{}] PANIC in load thread: {:?}", DB_NAME, e);
            log_error(&msg);
            let mut db = DB.lock();
            db.error_msg = msg;
            db.status = DbStatus::Error;
        }
    });
}

pub fn rebuild() {
    {
        let mut db = DB.lock();
        if matches!(db.status, DbStatus::Loading | DbStatus::Updating) {
            log_debug(&format!("[{}] rebuild skipped - already loading/updating", DB_NAME));
            return;
        }
        db.status = DbStatus::Loading;
        db.entries.clear();
        db.error_msg.clear();
        db.progress = None;
    }
    log_debug(&format!("[{}] Deleting cache and starting rebuild", DB_NAME));
    super::delete_cache(CACHE_FILE);

    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            fetch_from_api();
        });
        if let Err(e) = result {
            let msg = format!("[{}] PANIC in rebuild thread: {:?}", DB_NAME, e);
            log_error(&msg);
            let mut db = DB.lock();
            db.error_msg = msg;
            db.status = DbStatus::Error;
        }
    });
}

pub fn update() {
    {
        let db = DB.lock();
        if matches!(db.status, DbStatus::Loading | DbStatus::Updating) {
            return;
        }
        if db.status != DbStatus::Loaded {
            return;
        }
    }
    {
        let mut db = DB.lock();
        db.status = DbStatus::Updating;
        db.progress = None;
    }
    log_debug(&format!("[{}] Starting incremental update", DB_NAME));

    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            fetch_new_from_api();
        });
        if let Err(e) = result {
            let msg = format!("[{}] PANIC in update thread: {:?}", DB_NAME, e);
            log_error(&msg);
            let mut db = DB.lock();
            db.status = DbStatus::Loaded;
        }
    });
}

fn fetch_new_from_api() {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            log_error(&format!("[{}] Failed to create tokio runtime: {}", DB_NAME, e));
            let mut db = DB.lock();
            db.status = DbStatus::Loaded;
            return;
        }
    };

    let result = rt.block_on(async {
        let client = super::fetcher::make_client()?;
        let all_ids = super::fetcher::fetch_all_ids(&client, "/traits").await?;

        let existing_ids: HashSet<u32> = {
            let db = DB.lock();
            db.entries.iter().map(|e| e.id).collect()
        };

        let new_ids: Vec<u32> = all_ids.into_iter().filter(|id| !existing_ids.contains(id)).collect();

        if new_ids.is_empty() {
            log_debug(&format!("[{}] Already up to date", DB_NAME));
            return Ok(Vec::new());
        }

        log_debug(&format!("[{}] Fetching {} new entries", DB_NAME, new_ids.len()));
        let entries = super::fetcher::batch_fetch::<GwTrait>(
            &client,
            "/traits",
            &new_ids,
            &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            },
        )
        .await?;

        Ok::<Vec<GwTrait>, String>(entries)
    });

    let mut db = DB.lock();
    match result {
        Ok(new_entries) => {
            if !new_entries.is_empty() {
                log_debug(&format!("[{}] Update complete, {} new entries", DB_NAME, new_entries.len()));
                db.entries.extend(new_entries);
                super::save_to_cache(CACHE_FILE, &db.entries);
            }
            db.status = DbStatus::Loaded;
            db.progress = None;
        }
        Err(e) => {
            log_error(&format!("[{}] Update failed: {}", DB_NAME, e));
            db.status = DbStatus::Loaded;
            db.progress = None;
        }
    }
}

fn fetch_from_api() {
    log_debug(&format!("[{}] fetch_from_api starting", DB_NAME));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();

    let rt = match rt {
        Ok(rt) => rt,
        Err(e) => {
            log_error(&format!("[{}] Failed to create tokio runtime: {}", DB_NAME, e));
            let mut db = DB.lock();
            db.error_msg = e.to_string();
            db.status = DbStatus::Error;
            return;
        }
    };

    let result = rt.block_on(async {
        let client = super::fetcher::make_client()?;
        let ids = super::fetcher::fetch_all_ids(&client, "/traits").await?;

        log_debug(&format!("[{}] Fetching {} entries from API", DB_NAME, ids.len()));
        let entries = super::fetcher::batch_fetch::<GwTrait>(
            &client,
            "/traits",
            &ids,
            &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            },
        )
        .await?;

        Ok::<Vec<GwTrait>, String>(entries)
    });

    let mut db = DB.lock();
    match result {
        Ok(entries) => {
            log_debug(&format!("[{}] API fetch complete, {} entries. Saving cache...", DB_NAME, entries.len()));
            super::save_to_cache(CACHE_FILE, &entries);
            db.entries = entries;
            db.status = DbStatus::Loaded;
            db.progress = None;
            log_debug(&format!("[{}] Done, status=Loaded", DB_NAME));
        }
        Err(e) => {
            log_error(&format!("[{}] API fetch failed: {}", DB_NAME, e));
            db.error_msg = e;
            db.status = DbStatus::Error;
            db.progress = None;
        }
    }
}

pub fn search(query: &str, max_results: usize) -> Vec<(u32, String)> {
    let db = DB.lock();
    if db.entries.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    db.entries
        .iter()
        .filter(|e| e.matches_search(&query_lower))
        .take(max_results)
        .map(|e| (e.id(), e.display_label()))
        .collect()
}

fn status_str(s: DbStatus) -> &'static str {
    match s {
        DbStatus::NotLoaded => "NotLoaded",
        DbStatus::Loading => "Loading",
        DbStatus::Loaded => "Loaded",
        DbStatus::Updating => "Updating",
        DbStatus::Error => "Error",
    }
}
