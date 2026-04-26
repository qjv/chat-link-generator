use std::collections::HashSet;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{log_debug, log_error, DbEntry, DbStatus, GenericDatabase};

const CACHE_FILE: &str = "db_api_items.json";
const OLD_CACHE_FILE: &str = "items_filtered.json";
const DB_NAME: &str = "items";

#[derive(Serialize, Deserialize, Clone)]
pub struct Item {
    pub id: u32,
    pub name: String,
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(default)]
    pub detail_type: String,
}

impl DbEntry for Item {
    fn id(&self) -> u32 {
        self.id
    }

    fn display_label(&self) -> String {
        if self.detail_type.is_empty() {
            format!("[{}] {} ({})", self.id, self.name, self.item_type)
        } else {
            format!(
                "[{}] {} ({}/{})",
                self.id, self.name, self.item_type, self.detail_type
            )
        }
    }

    fn matches_search(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query)
    }
}

impl Item {
    pub fn is_upgrade(&self) -> bool {
        self.item_type == "UpgradeComponent"
    }

    pub fn upgrade_subtype(&self) -> &str {
        if self.detail_type.is_empty() {
            "Unknown"
        } else {
            &self.detail_type
        }
    }
}

// --- Filters ---

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ItemFilter {
    All,
    UpgradeComponent,
    Weapon,
    Armor,
    Trinket,
    Back,
    Consumable,
    Other,
}

impl ItemFilter {
    pub const ALL: &[ItemFilter] = &[
        ItemFilter::All,
        ItemFilter::UpgradeComponent,
        ItemFilter::Weapon,
        ItemFilter::Armor,
        ItemFilter::Trinket,
        ItemFilter::Back,
        ItemFilter::Consumable,
        ItemFilter::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ItemFilter::All => "All",
            ItemFilter::UpgradeComponent => "Upgrade Component",
            ItemFilter::Weapon => "Weapon",
            ItemFilter::Armor => "Armor",
            ItemFilter::Trinket => "Trinket",
            ItemFilter::Back => "Back",
            ItemFilter::Consumable => "Consumable",
            ItemFilter::Other => "Other",
        }
    }

    pub fn matches(self, item: &Item) -> bool {
        match self {
            ItemFilter::All => true,
            ItemFilter::UpgradeComponent => item.item_type == "UpgradeComponent",
            ItemFilter::Weapon => item.item_type == "Weapon",
            ItemFilter::Armor => item.item_type == "Armor",
            ItemFilter::Trinket => item.item_type == "Trinket",
            ItemFilter::Back => item.item_type == "Back",
            ItemFilter::Consumable => item.item_type == "Consumable",
            ItemFilter::Other => !matches!(
                item.item_type.as_str(),
                "UpgradeComponent" | "Weapon" | "Armor" | "Trinket" | "Back" | "Consumable"
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpgradeFilter {
    All,
    Rune,
    Sigil,
    Gem,
    Default,
}

impl UpgradeFilter {
    pub const ALL: &[UpgradeFilter] = &[
        UpgradeFilter::All,
        UpgradeFilter::Rune,
        UpgradeFilter::Sigil,
        UpgradeFilter::Gem,
        UpgradeFilter::Default,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UpgradeFilter::All => "All Upgrades",
            UpgradeFilter::Rune => "Rune",
            UpgradeFilter::Sigil => "Sigil",
            UpgradeFilter::Gem => "Gem",
            UpgradeFilter::Default => "Default (Infusion/Jewel)",
        }
    }

    pub fn matches(self, item: &Item) -> bool {
        if !item.is_upgrade() {
            return false;
        }
        match self {
            UpgradeFilter::All => true,
            UpgradeFilter::Rune => item.upgrade_subtype() == "Rune",
            UpgradeFilter::Sigil => item.upgrade_subtype() == "Sigil",
            UpgradeFilter::Gem => item.upgrade_subtype() == "Gem",
            UpgradeFilter::Default => item.upgrade_subtype() == "Default",
        }
    }
}

// --- Raw API types for fetching ---

#[derive(Deserialize)]
struct RawItem {
    id: u32,
    name: String,
    #[serde(rename = "type", default)]
    item_type: String,
    #[serde(default)]
    details: Option<RawItemDetails>,
}

#[derive(Deserialize)]
struct RawItemDetails {
    #[serde(rename = "type", default)]
    detail_type: Option<String>,
}

/// Old format item from the hosted JSON (for migration).
#[derive(Deserialize)]
struct OldItem {
    id: u32,
    name: String,
    #[serde(rename = "type", default)]
    item_type: String,
    #[serde(default)]
    details: Option<OldItemDetails>,
}

#[derive(Deserialize)]
struct OldItemDetails {
    #[serde(rename = "type", default)]
    detail_type: Option<String>,
}

pub static DB: Lazy<Mutex<GenericDatabase<Item>>> =
    Lazy::new(|| Mutex::new(GenericDatabase::default()));

pub fn ensure_loaded() {
    {
        let db = DB.lock();
        log_debug(&format!(
            "[{}] ensure_loaded - status: {:?}",
            DB_NAME,
            status_str(db.status)
        ));
        if matches!(
            db.status,
            DbStatus::Loading | DbStatus::Loaded | DbStatus::Updating
        ) {
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
            // Try new cache
            if let Some(entries) = super::load_from_cache::<Item>(CACHE_FILE) {
                if !entries.is_empty() {
                    log_debug(&format!(
                        "[{}] Loaded {} entries from cache",
                        DB_NAME,
                        entries.len()
                    ));
                    let mut db = DB.lock();
                    db.entries = entries;
                    db.status = DbStatus::Loaded;
                    db.progress = None;
                    return;
                }
            }

            // Try migrating old cache
            log_debug(&format!("[{}] Trying old cache migration", DB_NAME));
            if try_migrate_old_cache() {
                return;
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

fn try_migrate_old_cache() -> bool {
    let Some(dir) = nexus::paths::get_addon_dir("chat_link_generator") else {
        log_debug(&format!("[{}] No addon dir for migration", DB_NAME));
        return false;
    };
    let old_path = dir.join(OLD_CACHE_FILE);
    if !old_path.exists() {
        log_debug(&format!("[{}] Old cache file not found", DB_NAME));
        return false;
    }

    log_debug(&format!("[{}] Found old cache, migrating...", DB_NAME));
    let data = match std::fs::read_to_string(&old_path) {
        Ok(d) => d,
        Err(e) => {
            log_error(&format!("[{}] Failed to read old cache: {}", DB_NAME, e));
            return false;
        }
    };

    let old_items: Vec<OldItem> = match serde_json::from_str(&data) {
        Ok(items) => items,
        Err(e) => {
            log_error(&format!("[{}] Failed to parse old cache: {}", DB_NAME, e));
            return false;
        }
    };

    if old_items.is_empty() {
        log_debug(&format!("[{}] Old cache was empty", DB_NAME));
        return false;
    }

    let entries: Vec<Item> = old_items
        .into_iter()
        .map(|old| Item {
            id: old.id,
            name: old.name,
            item_type: old.item_type,
            detail_type: old.details.and_then(|d| d.detail_type).unwrap_or_default(),
        })
        .collect();

    log_debug(&format!(
        "[{}] Migrated {} items from old cache, saving new format",
        DB_NAME,
        entries.len()
    ));
    super::save_to_cache(CACHE_FILE, &entries);

    let mut db = DB.lock();
    db.entries = entries;
    db.status = DbStatus::Loaded;
    db.progress = None;
    true
}

pub fn rebuild() {
    {
        let mut db = DB.lock();
        if matches!(db.status, DbStatus::Loading | DbStatus::Updating) {
            log_debug(&format!(
                "[{}] rebuild skipped - already loading/updating",
                DB_NAME
            ));
            return;
        }
        db.status = DbStatus::Loading;
        db.entries.clear();
        db.error_msg.clear();
        db.progress = None;
    }
    log_debug(&format!(
        "[{}] Deleting cache and starting rebuild",
        DB_NAME
    ));
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
            log_debug(&format!(
                "[{}] update skipped - already loading/updating",
                DB_NAME
            ));
            return;
        }
        if db.status != DbStatus::Loaded {
            log_debug(&format!("[{}] update skipped - not loaded yet", DB_NAME));
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
            db.error_msg = msg;
            db.status = DbStatus::Loaded; // keep existing data usable
        }
    });
}

fn fetch_new_from_api() {
    log_debug(&format!("[{}] fetch_new_from_api starting", DB_NAME));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();

    let rt = match rt {
        Ok(rt) => rt,
        Err(e) => {
            log_error(&format!(
                "[{}] Failed to create tokio runtime: {}",
                DB_NAME, e
            ));
            let mut db = DB.lock();
            db.status = DbStatus::Loaded;
            return;
        }
    };

    let result = rt.block_on(async {
        let client = super::fetcher::make_client()?;
        let all_ids = super::fetcher::fetch_all_ids(&client, "/items").await?;

        let existing_ids: HashSet<u32> = {
            let db = DB.lock();
            db.entries.iter().map(|e| e.id).collect()
        };

        let new_ids: Vec<u32> = all_ids
            .into_iter()
            .filter(|id| !existing_ids.contains(id))
            .collect();

        if new_ids.is_empty() {
            log_debug(&format!("[{}] Already up to date", DB_NAME));
            return Ok(Vec::new());
        }

        log_debug(&format!(
            "[{}] Fetching {} new entries",
            DB_NAME,
            new_ids.len()
        ));
        let batch = super::fetcher::batch_fetch::<RawItem>(
            &client,
            "/items",
            &new_ids,
            &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            },
        )
        .await;

        let entries: Vec<Item> = batch
            .entries
            .into_iter()
            .map(|raw| Item {
                id: raw.id,
                name: raw.name,
                item_type: raw.item_type,
                detail_type: raw.details.and_then(|d| d.detail_type).unwrap_or_default(),
            })
            .collect();

        if let Some(err) = batch.error {
            if entries.is_empty() {
                return Err(err);
            }
            log_error(&format!(
                "[{}] Partial fetch ({} entries): {}",
                DB_NAME,
                entries.len(),
                err
            ));
        }

        Ok::<Vec<Item>, String>(entries)
    });

    let mut db = DB.lock();
    match result {
        Ok(new_entries) => {
            if !new_entries.is_empty() {
                log_debug(&format!(
                    "[{}] Update complete, {} new entries",
                    DB_NAME,
                    new_entries.len()
                ));
                db.entries.extend(new_entries);
                super::save_to_cache(CACHE_FILE, &db.entries);
            }
            db.status = DbStatus::Loaded;
            db.progress = None;
        }
        Err(e) => {
            log_error(&format!("[{}] Update failed: {}", DB_NAME, e));
            db.status = DbStatus::Loaded; // keep existing data
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
            log_error(&format!(
                "[{}] Failed to create tokio runtime: {}",
                DB_NAME, e
            ));
            let mut db = DB.lock();
            db.error_msg = e.to_string();
            db.status = DbStatus::Error;
            return;
        }
    };

    let result = rt.block_on(async {
        let client = super::fetcher::make_client()?;
        let ids = super::fetcher::fetch_all_ids(&client, "/items").await?;

        log_debug(&format!(
            "[{}] Fetching {} entries from API",
            DB_NAME,
            ids.len()
        ));
        let batch =
            super::fetcher::batch_fetch::<RawItem>(&client, "/items", &ids, &|fetched, total| {
                let mut db = DB.lock();
                db.progress = Some((fetched, total));
            })
            .await;

        log_debug(&format!(
            "[{}] Converting {} raw items",
            DB_NAME,
            batch.entries.len()
        ));
        let entries: Vec<Item> = batch
            .entries
            .into_iter()
            .map(|raw| Item {
                id: raw.id,
                name: raw.name,
                item_type: raw.item_type,
                detail_type: raw.details.and_then(|d| d.detail_type).unwrap_or_default(),
            })
            .collect();

        if let Some(err) = batch.error {
            if entries.is_empty() {
                return Err(err);
            }
            log_error(&format!(
                "[{}] Partial fetch ({} entries): {}",
                DB_NAME,
                entries.len(),
                err
            ));
        }

        Ok::<Vec<Item>, String>(entries)
    });

    let mut db = DB.lock();
    match result {
        Ok(entries) => {
            log_debug(&format!(
                "[{}] API fetch complete, {} entries. Saving cache...",
                DB_NAME,
                entries.len()
            ));
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

pub fn search(query: &str, filter_index: usize, max_results: usize) -> Vec<(u32, String)> {
    let db = DB.lock();
    if db.entries.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let filter = ItemFilter::ALL
        .get(filter_index)
        .copied()
        .unwrap_or(ItemFilter::All);
    db.entries
        .iter()
        .filter(|e| filter.matches(e) && e.matches_search(&query_lower))
        .take(max_results)
        .map(|e| (e.id(), e.display_label()))
        .collect()
}

pub fn search_names(query: &str, filter_index: usize, max_results: usize) -> Vec<(u32, String)> {
    let db = DB.lock();
    if db.entries.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let filter = ItemFilter::ALL
        .get(filter_index)
        .copied()
        .unwrap_or(ItemFilter::All);
    db.entries
        .iter()
        .filter(|e| filter.matches(e) && e.matches_search(&query_lower))
        .take(max_results)
        .map(|e| (e.id, e.name.clone()))
        .collect()
}

pub fn search_upgrades(
    query: &str,
    upgrade_filter_index: usize,
    max_results: usize,
) -> Vec<(u32, String)> {
    let db = DB.lock();
    if db.entries.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let filter = UpgradeFilter::ALL
        .get(upgrade_filter_index)
        .copied()
        .unwrap_or(UpgradeFilter::All);
    db.entries
        .iter()
        .filter(|e| filter.matches(e) && e.matches_search(&query_lower))
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
