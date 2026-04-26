pub mod fetcher;
pub mod ingame_items;
pub mod items;
pub mod outfits;
pub mod pois;
pub mod recipes;
pub mod skills;
pub mod skins;
pub mod traits;

use std::sync::atomic::AtomicBool;
use std::sync::Once;

use crate::encoder::LinkType;

const LOG_TAG: &str = "Chat Link Generator";

/// Global shutdown flag — set to `true` on plugin unload so background threads
/// can bail out early instead of blocking the game.
pub static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static MIGRATE_API_CACHE_ONCE: Once = Once::new();

pub fn log_debug(msg: &str) {
    nexus::log::log(nexus::log::LogLevel::Info, LOG_TAG, msg);
}

pub fn log_error(msg: &str) {
    nexus::log::log(nexus::log::LogLevel::Critical, LOG_TAG, msg);
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DbStatus {
    NotLoaded,
    Loading,
    Loaded,
    Updating,
    Error,
}

pub struct GenericDatabase<T> {
    pub entries: Vec<T>,
    pub status: DbStatus,
    pub error_msg: String,
    pub progress: Option<(usize, usize)>,
}

impl<T> Default for GenericDatabase<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            status: DbStatus::NotLoaded,
            error_msg: String::new(),
            progress: None,
        }
    }
}

/// Trait that all database entry types implement.
pub trait DbEntry: Send + Sync + 'static {
    fn id(&self) -> u32;
    fn display_label(&self) -> String;
    fn matches_search(&self, query: &str) -> bool;
}

fn get_db_dir() -> Option<std::path::PathBuf> {
    nexus::paths::get_addon_dir("chat_link_generator")
}

fn get_cache_path(filename: &str) -> Option<std::path::PathBuf> {
    get_db_dir().map(|d| d.join(filename))
}

fn migrate_api_cache_filenames_once() {
    MIGRATE_API_CACHE_ONCE.call_once(|| {
        let Some(dir) = get_db_dir() else {
            return;
        };
        let pairs = [
            ("db_items.json", "db_api_items.json"),
            ("db_skills.json", "db_api_skills.json"),
            ("db_traits.json", "db_api_traits.json"),
            ("db_recipes.json", "db_api_recipes.json"),
            ("db_skins.json", "db_api_skins.json"),
            ("db_outfits.json", "db_api_outfits.json"),
            ("db_pois.json", "db_api_pois.json"),
        ];
        for (old, new) in pairs {
            let old_path = dir.join(old);
            let new_path = dir.join(new);
            if !old_path.exists() || new_path.exists() {
                continue;
            }
            match std::fs::rename(&old_path, &new_path) {
                Ok(_) => log_debug(&format!("[cache] migrated {} -> {}", old, new)),
                Err(e) => log_error(&format!(
                    "[cache] failed migrating {} -> {}: {}",
                    old, new, e
                )),
            }
        }
    });
}

pub fn load_from_cache<T: serde::de::DeserializeOwned>(filename: &str) -> Option<Vec<T>> {
    let path = get_cache_path(filename)?;
    if !path.exists() {
        log_debug(&format!("[cache] {} not found on disk", filename));
        return None;
    }
    log_debug(&format!("[cache] Reading {} from disk...", filename));
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            log_error(&format!("[cache] Failed to read {}: {}", filename, e));
            return None;
        }
    };
    log_debug(&format!(
        "[cache] Parsing {} ({} bytes)...",
        filename,
        data.len()
    ));
    match serde_json::from_str(&data) {
        Ok(v) => Some(v),
        Err(e) => {
            log_error(&format!("[cache] Failed to parse {}: {}", filename, e));
            None
        }
    }
}

pub fn save_to_cache<T: serde::Serialize>(filename: &str, entries: &[T]) {
    log_debug(&format!(
        "[cache] Saving {} ({} entries)...",
        filename,
        entries.len()
    ));
    if let Some(path) = get_cache_path(filename) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(entries) {
            Ok(json) => {
                log_debug(&format!(
                    "[cache] Serialized {} ({} bytes), writing...",
                    filename,
                    json.len()
                ));
                match std::fs::write(&path, json) {
                    Ok(_) => log_debug(&format!("[cache] {} saved successfully", filename)),
                    Err(e) => log_error(&format!("[cache] Failed to write {}: {}", filename, e)),
                }
            }
            Err(e) => log_error(&format!("[cache] Failed to serialize {}: {}", filename, e)),
        }
    } else {
        log_error(&format!("[cache] Could not resolve path for {}", filename));
    }
}

pub fn delete_cache(filename: &str) {
    if let Some(path) = get_cache_path(filename) {
        let _ = std::fs::remove_file(&path);
    }
}

/// Ensure the database for the given link type is loaded.
/// If not loaded, starts a background fetch.
pub fn ensure_loaded(link_type: LinkType) {
    migrate_api_cache_filenames_once();
    log_debug(&format!(
        "[db] ensure_loaded called for {}",
        link_type.name()
    ));
    match link_type {
        LinkType::Item => items::ensure_loaded(),
        LinkType::Skill => skills::ensure_loaded(),
        LinkType::Trait => traits::ensure_loaded(),
        LinkType::Recipe => recipes::ensure_loaded(),
        LinkType::Wardrobe => skins::ensure_loaded(),
        LinkType::Outfit => outfits::ensure_loaded(),
        LinkType::Map => pois::ensure_loaded(),
    }
}

pub fn ensure_all_loaded() {
    for &lt in LinkType::ALL {
        ensure_loaded(lt);
    }
}

/// Rebuild (delete cache + re-fetch) for the given link type.
pub fn rebuild(link_type: LinkType) {
    log_debug(&format!("[db] rebuild called for {}", link_type.name()));
    match link_type {
        LinkType::Item => items::rebuild(),
        LinkType::Skill => skills::rebuild(),
        LinkType::Trait => traits::rebuild(),
        LinkType::Recipe => recipes::rebuild(),
        LinkType::Wardrobe => skins::rebuild(),
        LinkType::Outfit => outfits::rebuild(),
        LinkType::Map => pois::rebuild(),
    }
}

/// Incrementally update (fetch only new IDs) for the given link type.
pub fn update(link_type: LinkType) {
    log_debug(&format!("[db] update called for {}", link_type.name()));
    match link_type {
        LinkType::Item => items::update(),
        LinkType::Skill => skills::update(),
        LinkType::Trait => traits::update(),
        LinkType::Recipe => recipes::update(),
        LinkType::Wardrobe => skins::update(),
        LinkType::Outfit => outfits::update(),
        LinkType::Map => pois::update(),
    }
}

pub fn update_all() {
    for &lt in LinkType::ALL {
        update(lt);
    }
}

/// Get the status for the given link type's database.
pub fn get_status(link_type: LinkType) -> (DbStatus, usize, String, Option<(usize, usize)>) {
    match link_type {
        LinkType::Item => {
            let db = items::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Skill => {
            let db = skills::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Trait => {
            let db = traits::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Recipe => {
            let db = recipes::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Wardrobe => {
            let db = skins::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Outfit => {
            let db = outfits::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
        LinkType::Map => {
            let db = pois::DB.lock();
            (
                db.status,
                db.entries.len(),
                db.error_msg.clone(),
                db.progress,
            )
        }
    }
}

/// Search the database for the given link type.
pub fn search(
    link_type: LinkType,
    query: &str,
    filter_index: usize,
    max_results: usize,
) -> Vec<(u32, String)> {
    match link_type {
        LinkType::Item => items::search(query, filter_index, max_results),
        LinkType::Skill => skills::search(query, max_results),
        LinkType::Trait => traits::search(query, max_results),
        LinkType::Recipe => recipes::search(query, max_results),
        LinkType::Wardrobe => skins::search(query, filter_index, max_results),
        LinkType::Outfit => outfits::search(query, max_results),
        LinkType::Map => pois::search(query, filter_index, max_results),
    }
}

/// Returns filter names for types that support filtering.
pub fn filter_names(link_type: LinkType) -> Vec<&'static str> {
    match link_type {
        LinkType::Item => items::ItemFilter::ALL.iter().map(|f| f.name()).collect(),
        LinkType::Wardrobe => skins::SkinFilter::ALL.iter().map(|f| f.name()).collect(),
        LinkType::Map => pois::PoiFilter::ALL.iter().map(|f| f.name()).collect(),
        _ => vec![],
    }
}

/// Rebuild all databases.
pub fn rebuild_all() {
    log_debug("[db] rebuild_all starting");
    for &lt in LinkType::ALL {
        log_debug(&format!("[db] rebuild_all -> rebuilding {}", lt.name()));
        rebuild(lt);
    }
    ingame_items::rebuild();
    log_debug("[db] rebuild_all done (threads spawned)");
}

/// Clear all caches (delete files but don't reload).
pub fn clear_all_caches() {
    log_debug("[db] clear_all_caches starting");
    delete_cache("db_api_items.json");
    delete_cache("db_ingame_items.json");
    delete_cache("db_ingame_items_changelog.json");
    delete_cache("db_ingame_item_hashes.json");
    delete_cache("db_ingame_item_names.json");
    delete_cache("db_ingame_item_name_failed.json");
    delete_cache("db_ingame_all_data.json");
    delete_cache("db_ingame_map_data.json");
    delete_cache("db_ingame_skill_data.json");
    delete_cache("db_ingame_trait_data.json");
    delete_cache("db_ingame_recipe_data.json");
    delete_cache("db_ingame_wardrobe_data.json");
    delete_cache("db_ingame_outfit_data.json");
    delete_cache("db_ingame_poi_hashes.json");
    delete_cache("db_ingame_skill_hashes.json");
    delete_cache("db_ingame_trait_hashes.json");
    delete_cache("db_ingame_recipe_hashes.json");
    delete_cache("db_ingame_wardrobe_hashes.json");
    delete_cache("db_ingame_outfit_hashes.json");
    delete_cache("db_ingame_poi_names.json");
    delete_cache("db_ingame_skill_names.json");
    delete_cache("db_ingame_trait_names.json");
    delete_cache("db_ingame_recipe_names.json");
    delete_cache("db_ingame_wardrobe_names.json");
    delete_cache("db_ingame_outfit_names.json");
    delete_cache("db_api_skills.json");
    delete_cache("db_api_traits.json");
    delete_cache("db_api_recipes.json");
    delete_cache("db_api_skins.json");
    delete_cache("db_api_outfits.json");
    delete_cache("db_api_pois.json");

    // Reset all database states
    {
        let mut db = items::DB.lock();
        *db = GenericDatabase::default();
    }
    ingame_items::reset_state_for_clear_all();
    {
        let mut db = skills::DB.lock();
        *db = GenericDatabase::default();
    }
    {
        let mut db = traits::DB.lock();
        *db = GenericDatabase::default();
    }
    {
        let mut db = recipes::DB.lock();
        *db = GenericDatabase::default();
    }
    {
        let mut db = skins::DB.lock();
        *db = GenericDatabase::default();
    }
    {
        let mut db = outfits::DB.lock();
        *db = GenericDatabase::default();
    }
    {
        let mut db = pois::DB.lock();
        *db = GenericDatabase::default();
    }
    log_debug("[db] clear_all_caches done");
}
