use std::collections::HashMap;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{DbEntry, DbStatus, GenericDatabase, log_debug, log_error};

const CACHE_FILE: &str = "db_pois.json";
const GW2_API_BASE: &str = "https://api.guildwars2.com/v2";
const CONTINENT_IDS: &[u32] = &[1, 2]; // Tyria, Mists
const DB_NAME: &str = "pois";

#[derive(Serialize, Deserialize, Clone)]
pub struct Poi {
    pub id: u32,
    pub name: String,
    pub poi_type: String,
}

impl DbEntry for Poi {
    fn id(&self) -> u32 {
        self.id
    }

    fn display_label(&self) -> String {
        format!("[{}] {} ({})", self.id, self.name, self.poi_type)
    }

    fn matches_search(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PoiFilter {
    All,
    Waypoint,
    Landmark,
    Vista,
    Unlock,
}

impl PoiFilter {
    pub const ALL: &[PoiFilter] = &[
        PoiFilter::All,
        PoiFilter::Waypoint,
        PoiFilter::Landmark,
        PoiFilter::Vista,
        PoiFilter::Unlock,
    ];

    pub fn name(self) -> &'static str {
        match self {
            PoiFilter::All => "All",
            PoiFilter::Waypoint => "Waypoint",
            PoiFilter::Landmark => "Landmark (POI)",
            PoiFilter::Vista => "Vista",
            PoiFilter::Unlock => "Unlock",
        }
    }

    pub fn matches(self, poi: &Poi) -> bool {
        match self {
            PoiFilter::All => true,
            PoiFilter::Waypoint => poi.poi_type == "waypoint",
            PoiFilter::Landmark => poi.poi_type == "landmark",
            PoiFilter::Vista => poi.poi_type == "vista",
            PoiFilter::Unlock => poi.poi_type == "unlock",
        }
    }
}

// --- API response types ---

#[derive(Deserialize)]
struct FloorResponse {
    #[serde(default)]
    regions: HashMap<String, RegionData>,
}

#[derive(Deserialize)]
struct RegionData {
    #[serde(default)]
    maps: HashMap<String, MapData>,
}

#[derive(Deserialize)]
struct MapData {
    #[serde(default)]
    points_of_interest: HashMap<String, RawPoi>,
}

#[derive(Deserialize)]
struct RawPoi {
    #[serde(default)]
    id: Option<u32>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type", default)]
    poi_type: Option<String>,
}

pub static DB: Lazy<Mutex<GenericDatabase<Poi>>> =
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
            if let Some(entries) = super::load_from_cache::<Poi>(CACHE_FILE) {
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

/// POIs use traversal-based fetching, so update just does a full rebuild.
pub fn update() {
    rebuild();
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
        let mut all_pois: HashMap<u32, Poi> = HashMap::new();
        let mut floors_done: usize = 0;
        let mut floors_total: usize = 0;

        // First, count total floors for progress
        let mut continent_floors: Vec<(u32, Vec<i32>)> = Vec::new();
        for &cid in CONTINENT_IDS {
            let url = format!("{}/continents/{}/floors", GW2_API_BASE, cid);
            log_debug(&format!("[{}] Fetching floor IDs for continent {}", DB_NAME, cid));
            let resp = client.get(&url).send().await.map_err(|e| {
                let msg = format!("[{}] HTTP error fetching floors for continent {}: {}", DB_NAME, cid, e);
                log_error(&msg);
                e.to_string()
            })?;
            let floor_ids: Vec<i32> = resp.json().await.map_err(|e| {
                let msg = format!("[{}] Parse error for continent {} floors: {}", DB_NAME, cid, e);
                log_error(&msg);
                e.to_string()
            })?;
            log_debug(&format!("[{}] Continent {} has {} floors", DB_NAME, cid, floor_ids.len()));
            floors_total += floor_ids.len();
            continent_floors.push((cid, floor_ids));
        }

        log_debug(&format!("[{}] Traversing {} total floors", DB_NAME, floors_total));

        // Traverse each floor
        for (cid, floor_ids) in &continent_floors {
            for &fid in floor_ids {
                let url = format!(
                    "{}/continents/{}/floors/{}",
                    GW2_API_BASE, cid, fid
                );

                let resp = client.get(&url).send().await;
                if let Ok(resp) = resp {
                    if let Ok(floor) = resp.json::<FloorResponse>().await {
                        for region in floor.regions.values() {
                            for map in region.maps.values() {
                                for raw_poi in map.points_of_interest.values() {
                                    let Some(id) = raw_poi.id else { continue };
                                    let Some(poi_type) = raw_poi.poi_type.as_deref() else {
                                        continue;
                                    };

                                    // Only keep relevant types
                                    if !matches!(
                                        poi_type,
                                        "waypoint" | "landmark" | "vista" | "unlock"
                                    ) {
                                        continue;
                                    }

                                    let name = raw_poi
                                        .name
                                        .as_deref()
                                        .unwrap_or("")
                                        .to_string();

                                    // Skip unnamed entries
                                    if name.is_empty() {
                                        continue;
                                    }

                                    all_pois.entry(id).or_insert_with(|| Poi {
                                        id,
                                        name,
                                        poi_type: poi_type.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }

                floors_done += 1;
                {
                    let mut db = DB.lock();
                    db.progress = Some((floors_done, floors_total));
                }

                // Small delay between floor requests
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }

        log_debug(&format!("[{}] Traversal complete, {} unique POIs found", DB_NAME, all_pois.len()));

        let mut entries: Vec<Poi> = all_pois.into_values().collect();
        entries.sort_by_key(|p| p.id);

        Ok::<Vec<Poi>, String>(entries)
    });

    let mut db = DB.lock();
    match result {
        Ok(entries) => {
            log_debug(&format!("[{}] Fetch complete, {} entries. Saving cache...", DB_NAME, entries.len()));
            super::save_to_cache(CACHE_FILE, &entries);
            db.entries = entries;
            db.status = DbStatus::Loaded;
            db.progress = None;
            log_debug(&format!("[{}] Done, status=Loaded", DB_NAME));
        }
        Err(e) => {
            log_error(&format!("[{}] Fetch failed: {}", DB_NAME, e));
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
    let filter = PoiFilter::ALL.get(filter_index).copied().unwrap_or(PoiFilter::All);
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
