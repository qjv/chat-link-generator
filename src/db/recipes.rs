use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{DbEntry, DbStatus, GenericDatabase, log_debug, log_error};

const CACHE_FILE: &str = "db_recipes.json";
const DB_NAME: &str = "recipes";

/// What we store in the cache and use at runtime.
#[derive(Serialize, Deserialize, Clone)]
pub struct Recipe {
    pub id: u32,
    pub output_item_name: String,
    #[serde(rename = "type", default)]
    pub recipe_type: String,
}

impl DbEntry for Recipe {
    fn id(&self) -> u32 {
        self.id
    }

    fn display_label(&self) -> String {
        format!("[{}] {} ({})", self.id, self.output_item_name, self.recipe_type)
    }

    fn matches_search(&self, query: &str) -> bool {
        self.output_item_name.to_lowercase().contains(query)
    }
}

/// Raw recipe from the API (before resolving item names).
#[derive(Deserialize)]
struct RawRecipe {
    id: u32,
    output_item_id: u32,
    #[serde(rename = "type", default)]
    recipe_type: String,
}

/// Minimal item response for name resolution.
#[derive(Deserialize)]
struct ItemName {
    id: u32,
    name: String,
}

pub static DB: Lazy<Mutex<GenericDatabase<Recipe>>> =
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
            if let Some(entries) = super::load_from_cache::<Recipe>(CACHE_FILE) {
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
        let all_ids = super::fetcher::fetch_all_ids(&client, "/recipes").await?;

        let existing_ids: HashSet<u32> = {
            let db = DB.lock();
            db.entries.iter().map(|e| e.id).collect()
        };

        let new_ids: Vec<u32> = all_ids.into_iter().filter(|id| !existing_ids.contains(id)).collect();

        if new_ids.is_empty() {
            log_debug(&format!("[{}] Already up to date", DB_NAME));
            return Ok(Vec::new());
        }

        log_debug(&format!("[{}] Fetching {} new recipes", DB_NAME, new_ids.len()));
        let batch = super::fetcher::batch_fetch::<RawRecipe>(
            &client,
            "/recipes",
            &new_ids,
            &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            },
        )
        .await;

        if let Some(ref err) = batch.error {
            if batch.entries.is_empty() {
                return Err(err.clone());
            }
            log_error(&format!("[{}] Partial fetch ({} entries): {}", DB_NAME, batch.entries.len(), err));
        }
        let raw_recipes = batch.entries;

        // Collect unique output_item_ids and fetch their names
        let unique_item_ids: Vec<u32> = {
            let set: HashSet<u32> = raw_recipes.iter().map(|r| r.output_item_id).collect();
            set.into_iter().collect()
        };

        log_debug(&format!("[{}] Resolving {} item names for new recipes", DB_NAME, unique_item_ids.len()));
        let item_names = super::fetcher::batch_fetch_lenient::<ItemName>(
            &client,
            "/items",
            &unique_item_ids,
            &|_fetched, _total| {},
        )
        .await?;

        let name_map: HashMap<u32, String> = item_names
            .into_iter()
            .map(|i| (i.id, i.name))
            .collect();

        let entries: Vec<Recipe> = raw_recipes
            .into_iter()
            .filter_map(|r| {
                let output_item_name = name_map
                    .get(&r.output_item_id)
                    .cloned()
                    .unwrap_or_else(|| format!("Item #{}", r.output_item_id));
                Some(Recipe {
                    id: r.id,
                    output_item_name,
                    recipe_type: r.recipe_type,
                })
            })
            .collect();

        Ok::<Vec<Recipe>, String>(entries)
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

        // Phase 1: Fetch all raw recipes
        log_debug(&format!("[{}] Phase 1: Fetching recipe IDs", DB_NAME));
        let ids = super::fetcher::fetch_all_ids(&client, "/recipes").await?;
        let total_recipes = ids.len();
        log_debug(&format!("[{}] Phase 1: Fetching {} recipes", DB_NAME, total_recipes));

        let batch = super::fetcher::batch_fetch::<RawRecipe>(
            &client,
            "/recipes",
            &ids,
            &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            },
        )
        .await;

        if let Some(ref err) = batch.error {
            if batch.entries.is_empty() {
                return Err(err.clone());
            }
            log_error(&format!("[{}] Partial fetch ({} entries): {}", DB_NAME, batch.entries.len(), err));
        }
        let raw_recipes = batch.entries;

        // Phase 2: Collect unique output_item_ids and fetch their names
        let unique_item_ids: Vec<u32> = {
            let set: HashSet<u32> = raw_recipes.iter().map(|r| r.output_item_id).collect();
            set.into_iter().collect()
        };
        log_debug(&format!("[{}] Phase 2: Resolving {} unique item names", DB_NAME, unique_item_ids.len()));

        {
            let mut db = DB.lock();
            db.progress = Some((total_recipes, total_recipes));
        }

        let item_names = super::fetcher::batch_fetch_lenient::<ItemName>(
            &client,
            "/items",
            &unique_item_ids,
            &|_fetched, _total| {},
        )
        .await?;

        let name_map: HashMap<u32, String> = item_names
            .into_iter()
            .map(|i| (i.id, i.name))
            .collect();

        // Phase 3: Join
        log_debug(&format!("[{}] Phase 3: Joining recipes with item names", DB_NAME));
        let entries: Vec<Recipe> = raw_recipes
            .into_iter()
            .map(|r| Recipe {
                id: r.id,
                output_item_name: name_map
                    .get(&r.output_item_id)
                    .cloned()
                    .unwrap_or_else(|| format!("Item #{}", r.output_item_id)),
                recipe_type: r.recipe_type,
            })
            .collect();

        Ok::<Vec<Recipe>, String>(entries)
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
