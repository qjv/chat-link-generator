use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memtools::{Pattern, PatternScan};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::config::RUNTIME_CONFIG;
use crate::db::{self, DbStatus};
use crate::encoder;

const CACHE_FILE: &str = "db_ingame_items.json";
const HASHES_CACHE_FILE: &str = "db_ingame_item_hashes.json";
const NAMES_CACHE_FILE: &str = "db_ingame_item_names.json";
const NAME_FAILED_CACHE_FILE: &str = "db_ingame_item_name_failed.json";
const LEGACY_GAME_TYPE_DATA_FILE: &str = "db_ingame_all_data.json";
const API_ITEMS_CACHE_FILE: &str = "db_api_items.json";

const LOG_PREFIX: &str = "[ingame-items]";

const ITEM_CONTENT_TYPE: u32 = 34; // EContentType::ItemDef
const PROP_CTX_CONTENT_CTX: usize = 0xE0;
const CONTENT_VT_COUNT_CONTENT_DEFS: usize = 0x40;
const CONTENT_VT_ITERATE_CONTENT_DEFS: usize = 0x48;
const CONTENT_VT_GET_CONTENT_BY_INDEX: usize = 0x58;

const ITEM_DEF_ID: usize = 0x28;
const ITEM_DEF_TYPE: usize = 0x2C;
const ITEM_DEF_SUBDEF: usize = 0x30;
const ITEM_DEF_RARITY: usize = 0x60;
const ITEM_DEF_REQUIRED_LEVEL: usize = 0x74;
const ITEM_DEF_NAME_HASH: usize = 0x80;
const ITEM_DEF_DESCRIPTION_HASH: usize = 0x84;
const ITEM_DEF_VENDOR_VALUE: usize = 0x88;
const ITEM_DEF_IS_GEMSTORE: usize = 0xB4;
const ITEM_UPGRADE_TEXT_HASH1: usize = 0x60;
const ITEM_UPGRADE_TEXT_HASH2: usize = 0x70;
const ITEM_UPGRADE_BASE_ITEM_ID: usize = 0xA0;
const CONTENT_DEF_TYPE: usize = 0x10;
const CONTENT_DEF_INDEX: usize = 0x14;
const POI_DEF_ID: usize = 0x28;
const POI_DEF_MAP_PTR: usize = 0x38;
const POI_DEF_NAME_HASH: usize = 0x40;
const POI_DEF_TYPE: usize = 0x44;
const MAP_DEF_NAME_HASH: usize = 0x168;
const SKIN_DEF_RARITY: usize = 0x40;
const SKIN_DEF_FLAGS: usize = 0x50;
const SKIN_DEF_TYPE: usize = 0x80;
const MAP_DEF_CONTENT_TYPE: u32 = 44; // EContentType::MapDef

const PROP_CTX_PATTERN: &str =
    "8B 0D ?? ?? ?? ?? 65 48 8B 04 25 58 00 00 00 BA ?? ?? ?? ?? 48 8B 04 C8 48 8B 04 02 C3";
const PROP_CTX_PATTERN_FALLBACK: &str =
    "8B 15 ?? ?? ?? ?? 65 48 8B 04 25 58 00 00 00 41 B8 ?? ?? ?? ?? 48 8B 04 D0 49 89 0C 00 C3";
const MAIN_THREAD_PATTERN: &str =
    "E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? 85 C9 75 08 89 05 ?? ?? ?? ?? EB 1D 3B C8 74 19";
const RESOLVE_TEXT_HASH_PATTERN: &str =
    "89 54 24 10 4C 89 44 24 18 4C 89 4C 24 20 53 57 48 83 EC 48 8B D9 E8 ?? ?? ?? ?? 48 8B 48";
const DECODE_TEXT_PATTERN: &str =
    "48 89 6C 24 10 48 89 74 24 18 57 48 83 EC 20 49 8B E8 48 8B F2 48 8B F9 48 85 C9 75 19";

const ITEM_SCAN_BOOTSTRAP_TAIL: u32 = 50_000;
const ITEM_SCAN_PER_TICK: usize = 12;
const ITEM_SCAN_BUDGET_MS: u64 = 1;
const DECODES_PER_TICK: usize = 12;
const NAME_PARSE_BUDGET_MS: u64 = 1;
const GAME_DATA_PER_TICK: usize = 24;
const MAP_GAME_DATA_PER_TICK: usize = 1;
const WARDROBE_GAME_DATA_PER_TICK: usize = 4;
const NAME_SAVE_EVERY: usize = 2000;
const NAME_SAVE_INTERVAL_MS: u64 = 2000;
const GAME_DATA_SAVE_EVERY: usize = 2000;
const GAME_DATA_COOLDOWN_MS: u64 = 3;
const MAP_GAME_DATA_COOLDOWN_MS: u64 = 16;
const NAME_DECODE_COOLDOWN_MS: u64 = 1;
const NAME_DECODE_MAX_RETRIES: u8 = 10;
const WARDROBE_NAME_DECODE_MAX_RETRIES: u8 = 64;
const WARDROBE_NAME_RESOLVE_ATTEMPTS: usize = 4;
const GAME_TYPE_NAME_DECODE_MAX_RETRIES: u8 = 3;
const MAX_TEXT_HASH_VALUE: u32 = 0x0100_0000;

#[derive(Serialize, Deserialize, Clone)]
pub struct InGameItem {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub item_type_code: u32,
    pub rarity_code: u32,
    pub required_level: u32,
    pub vendor_value: u32,
    pub is_gemstore: bool,
    #[serde(default)]
    pub default_skin_id: u32,
    #[serde(default)]
    pub upgrade_name: String,
    pub chat_link: String,
    pub in_api: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ItemHashEntry {
    pub id: u32,
    #[serde(default)]
    pub content_index: u32,
    pub name_hash: u32,
    pub description_hash: u32,
    #[serde(default)]
    pub upgrade_name_hash: u32,
    #[serde(default)]
    pub base_upgrade_item_id: u32,
    pub last_seen_unix: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ItemNameEntry {
    pub id: u32,
    pub name: String,
    pub last_seen_unix: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ItemNameFailedEntry {
    pub id: u32,
    pub name_hash: u32,
    pub last_seen_unix: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GameTypeDataEntry {
    pub link_type: String,
    pub id: u32,
    pub name: String,
    pub in_api: bool,
    #[serde(default)]
    pub name_hash: u32,
    #[serde(default)]
    pub description_hash: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub skin_rarity_code: u32,
    #[serde(default)]
    pub skin_flags_code: u32,
    #[serde(default)]
    pub skin_type_code: u32,
    #[serde(default)]
    pub poi_type_code: u32,
    #[serde(default)]
    pub map_name_hash: u32,
    #[serde(default)]
    pub map_name: String,
    pub last_seen_unix: u64,
}

#[derive(Clone)]
pub struct SearchResult {
    pub id: u32,
    pub name: String,
    pub in_api: bool,
}

#[derive(Clone)]
pub struct ContentOffsetProbeRow {
    pub offset: usize,
    pub raw_u32: u32,
    pub is_hash_candidate: bool,
    pub candidate_preview: String,
}

#[derive(Clone)]
pub struct ContentProbeDebugInfo {
    pub link_type: encoder::LinkType,
    pub id: u32,
    pub content_type: u32,
    pub resolved_content_type: u32,
    pub content_ptr: u64,
    pub content_def_type: u32,
    pub content_def_index: u32,
    pub known_name_offset: Option<usize>,
    pub rows: Vec<ContentOffsetProbeRow>,
    pub subdef_ptr: u64,
    pub subdef_rows: Vec<ContentOffsetProbeRow>,
}

#[derive(Clone)]
pub struct DebugResolveResult {
    pub link_type: encoder::LinkType,
    pub id: u32,
    pub resolved_content_type: u32,
    pub offset: usize,
    pub raw_u32: u32,
    pub coded_text: String,
    pub decoded_text: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum JobMode {
    Rebuild,
    Update,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NameParsePhase {
    All,
    Equipment,
    Upgrade,
}

struct ScanJob {
    mode: JobMode,
    start_id: u32,
    current_id: u32,
    end_id: u32,
    last_found_id: u32,
    trailing_gap: u32,
    processed: usize,
    added: usize,
}

struct NameParseJob {
    ids: Vec<u32>,
    cursor: usize,
    phase: NameParsePhase,
    equipment_failed_ids: Vec<u32>,
    upgrade_failed_ids: Vec<u32>,
    decoded: usize,
    pending_flush: usize,
    retry_counts: HashMap<u32, u8>,
    finalized_ids: HashSet<u32>,
    base_total: usize,
    base_finalized: usize,
    include_descriptions: bool,
    full_rebuild: bool,
    next_decode_at: Instant,
    last_flush_at: Instant,
}

struct GameTypeNameParseJob {
    link_type: encoder::LinkType,
    ids: Vec<u32>,
    cursor: usize,
    decoded: usize,
    pending_flush: usize,
    retry_counts: HashMap<u32, u8>,
    finalized_ids: HashSet<u32>,
    base_total: usize,
    base_finalized: usize,
    full_rebuild: bool,
    next_decode_at: Instant,
    last_flush_at: Instant,
}

struct MapNameParseJob {
    hashes: Vec<u32>,
    ids_by_hash: HashMap<u32, Vec<u32>>,
    cursor: usize,
    decoded: usize,
    pending_flush: usize,
    retry_counts: HashMap<u32, u8>,
    finalized_ids: HashSet<u32>,
    base_total: usize,
    base_finalized: usize,
    full_rebuild: bool,
    next_decode_at: Instant,
    last_flush_at: Instant,
}

struct GameTypeTask {
    link_type: encoder::LinkType,
    content_type: u32,
    id: u32,
    api_name: String,
}

struct GameTypeBuildJob {
    tasks: Vec<GameTypeTask>,
    cursor: usize,
    added: usize,
    pending_flush: usize,
    touched_item_cache: bool,
    paused: bool,
    next_step_at: Instant,
    map_direct_scan: bool,
    map_content_type: u32,
    map_iter_index: u32,
    map_processed: usize,
    map_total: usize,
    map_api_names: HashMap<u32, String>,
}

#[derive(Default)]
struct ScanPointers {
    prop_ctx_getter: *mut u8,
    main_thread_match: *mut u8,
    resolve_text_hash_fn: *mut u8,
    decode_text_fn: *mut u8,
}

unsafe impl Send for ScanPointers {}
unsafe impl Sync for ScanPointers {}

struct State {
    entries: Vec<InGameItem>,
    entry_index: HashMap<u32, usize>,
    status: DbStatus,
    error_msg: String,
    progress: Option<(usize, usize)>,
    api_ids: HashSet<u32>,
    api_names: HashMap<u32, String>,
    pointers: ScanPointers,
    scan: Option<ScanJob>,
    hashes: HashMap<u32, ItemHashEntry>,
    names: HashMap<u32, ItemNameEntry>,
    name_failed: HashMap<u32, ItemNameFailedEntry>,
    decoded_text_by_hash: HashMap<u32, String>,
    game_type_data: HashMap<String, HashMap<u32, GameTypeDataEntry>>,
    game_type_hashes: HashMap<String, HashMap<u32, ItemHashEntry>>,
    game_type_names: HashMap<String, HashMap<u32, ItemNameEntry>>,
    discovered_name_hash_offsets: HashMap<String, usize>,
    discovered_id_offsets: HashMap<String, usize>,
    mapdef_name_hash_by_ptr: HashMap<usize, u32>,
    name_job: Option<NameParseJob>,
    game_type_job: Option<GameTypeBuildJob>,
    game_type_name_job: Option<GameTypeNameParseJob>,
    map_name_job: Option<MapNameParseJob>,
    debug_resolve_job: Option<(encoder::LinkType, u32, Option<u32>, usize)>,
    debug_resolve_hash_job: Option<(encoder::LinkType, u32, usize, u32, Option<u64>)>,
    debug_resolve_result: Option<Result<DebugResolveResult, String>>,
    name_parse_paused: bool,
    loaded_cache: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            entry_index: HashMap::new(),
            status: DbStatus::NotLoaded,
            error_msg: String::new(),
            progress: None,
            api_ids: HashSet::new(),
            api_names: HashMap::new(),
            pointers: ScanPointers::default(),
            scan: None,
            hashes: HashMap::new(),
            names: HashMap::new(),
            name_failed: HashMap::new(),
            decoded_text_by_hash: HashMap::new(),
            game_type_data: HashMap::new(),
            game_type_hashes: HashMap::new(),
            game_type_names: HashMap::new(),
            discovered_name_hash_offsets: HashMap::new(),
            discovered_id_offsets: HashMap::new(),
            mapdef_name_hash_by_ptr: HashMap::new(),
            name_job: None,
            game_type_job: None,
            game_type_name_job: None,
            map_name_job: None,
            debug_resolve_job: None,
            debug_resolve_hash_job: None,
            debug_resolve_result: None,
            name_parse_paused: false,
            loaded_cache: false,
        }
    }
}

impl State {
    fn upsert_entry(
        &mut self,
        mut incoming: InGameItem,
        content_index: u32,
        name_hash: u32,
        description_hash: u32,
        upgrade_name_hash: u32,
        base_upgrade_item_id: u32,
    ) {
        let id = incoming.id;
        if let Some(saved) = self.names.get(&id) {
            if !saved.name.trim().is_empty() {
                incoming.name = saved.name.clone();
            }
        }

        if let Some(existing_idx) = self.entry_index.get(&incoming.id).copied() {
            if let Some(existing) = self.entries.get_mut(existing_idx) {
                if is_placeholder_name(&incoming.name) && !is_placeholder_name(&existing.name) {
                    incoming.name = existing.name.clone();
                }
                *existing = incoming;
            }
        } else {
            let new_idx = self.entries.len();
            self.entries.push(incoming);
            self.entry_index.insert(self.entries[new_idx].id, new_idx);
        }

        self.hashes.insert(
            id,
            ItemHashEntry {
                id,
                content_index,
                name_hash,
                description_hash,
                upgrade_name_hash,
                base_upgrade_item_id,
                last_seen_unix: now_unix(),
            },
        );
    }
}

fn has_active_unpaused_job(state: &State) -> bool {
    state.scan.is_some()
        || (state.name_job.is_some() && !state.name_parse_paused)
        || state.game_type_name_job.is_some()
        || state.map_name_job.is_some()
        || state
            .game_type_job
            .as_ref()
            .map(|j| !j.paused)
            .unwrap_or(false)
}

fn clear_paused_jobs(state: &mut State) {
    if state.name_parse_paused && state.name_job.is_some() {
        state.name_job = None;
        state.name_parse_paused = false;
    }
    if state
        .game_type_job
        .as_ref()
        .map(|j| j.paused)
        .unwrap_or(false)
    {
        state.game_type_job = None;
    }
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| Mutex::new(State::default()));
static AUTO_UPDATE_TRIGGERED: AtomicBool = AtomicBool::new(false);
static DECODED_TEXT: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

pub fn get_status() -> (DbStatus, usize, String, Option<(usize, usize)>) {
    let s = STATE.lock();
    (s.status, s.entries.len(), s.error_msg.clone(), s.progress)
}

pub fn reset_state_for_clear_all() {
    *STATE.lock() = State::default();
}

pub fn ensure_loaded() {
    let mut s = STATE.lock();
    if s.loaded_cache {
        return;
    }

    // Legacy cleanup: changelog file is no longer used.
    delete_cache_local("db_ingame_items_changelog.json");

    let (api_ids, api_names) = load_api_index();
    s.api_ids = api_ids;
    s.api_names = api_names;

    if let Some(entries) = load_cache::<Vec<InGameItem>>(CACHE_FILE) {
        s.entries = entries;
    }
    if let Some(hashes) = load_cache::<Vec<ItemHashEntry>>(HASHES_CACHE_FILE) {
        s.hashes = hashes.into_iter().map(|h| (h.id, h)).collect();
    }
    if let Some(names) = load_cache::<Vec<ItemNameEntry>>(NAMES_CACHE_FILE) {
        s.names = names
            .into_iter()
            .filter(|n| !is_unresolved_decoded_text(&n.name))
            .map(|n| (n.id, n))
            .collect();
    }
    if let Some(failed) = load_cache::<Vec<ItemNameFailedEntry>>(NAME_FAILED_CACHE_FILE) {
        s.name_failed = failed.into_iter().map(|f| (f.id, f)).collect();
    }
    for &lt in encoder::LinkType::ALL {
        if lt == encoder::LinkType::Item {
            continue;
        }
        let hash_file = game_type_hashes_file_for_link_type(lt);
        if let Some(rows) = load_cache::<Vec<ItemHashEntry>>(hash_file) {
            let key = lt.name().to_string();
            let bucket = s.game_type_hashes.entry(key).or_default();
            for row in rows {
                bucket.insert(row.id, row);
            }
        }
        let names_file = game_type_names_file_for_link_type(lt);
        if let Some(rows) = load_cache::<Vec<ItemNameEntry>>(names_file) {
            let key = lt.name().to_string();
            let bucket = s.game_type_names.entry(key).or_default();
            for row in rows {
                if is_unresolved_decoded_text(&row.name) {
                    continue;
                }
                bucket.insert(row.id, row);
            }
        }
        let file = game_type_data_file_for_link_type(lt);
        if let Some(rows) = load_cache::<Vec<GameTypeDataEntry>>(file) {
            let key = lt.name().to_string();
            let saved_names = s.game_type_names.get(&key);
            let mut normalized_rows = Vec::with_capacity(rows.len());
            for mut row in rows {
                if is_unresolved_decoded_text(&row.name) {
                    row.name.clear();
                }
                if row.name.trim().is_empty() {
                    if let Some(saved) = saved_names
                        .and_then(|m| m.get(&row.id))
                        .map(|n| n.name.clone())
                    {
                        row.name = saved;
                    }
                }
                normalized_rows.push(row);
            }
            let bucket = s.game_type_data.entry(key).or_default();
            for row in normalized_rows {
                bucket.insert(row.id, row);
            }
        }
    }
    if s.game_type_data.is_empty() {
        if let Some(game_data) = load_cache::<Vec<GameTypeDataEntry>>(LEGACY_GAME_TYPE_DATA_FILE) {
            let mut grouped: HashMap<String, HashMap<u32, GameTypeDataEntry>> = HashMap::new();
            for entry in game_data {
                grouped
                    .entry(entry.link_type.clone())
                    .or_default()
                    .insert(entry.id, entry);
            }
            s.game_type_data = grouped;
            let snapshot: Vec<(String, Vec<GameTypeDataEntry>)> = s
                .game_type_data
                .iter()
                .map(|(k, rows)| (k.clone(), rows.values().cloned().collect()))
                .collect();
            for (k, rows) in snapshot {
                for row in rows {
                    if row.name_hash != 0 {
                        s.game_type_hashes.entry(k.clone()).or_default().insert(
                            row.id,
                            ItemHashEntry {
                                id: row.id,
                                content_index: row.id,
                                name_hash: row.name_hash,
                                description_hash: row.description_hash,
                                upgrade_name_hash: 0,
                                base_upgrade_item_id: 0,
                                last_seen_unix: row.last_seen_unix,
                            },
                        );
                    }
                    if !row.name.trim().is_empty() {
                        if is_unresolved_decoded_text(&row.name) {
                            continue;
                        }
                        s.game_type_names.entry(k.clone()).or_default().insert(
                            row.id,
                            ItemNameEntry {
                                id: row.id,
                                name: row.name.clone(),
                                last_seen_unix: row.last_seen_unix,
                            },
                        );
                    }
                }
            }
            save_game_type_data_cache(&s.game_type_data, &s.game_type_hashes, &s.game_type_names);
            delete_cache_local(LEGACY_GAME_TYPE_DATA_FILE);
        }
    }

    apply_saved_names(&mut s);
    sanitize_loaded_names(&mut s);

    rebuild_entry_index_in_state(&mut s);

    s.status = if s.entries.is_empty() {
        DbStatus::NotLoaded
    } else {
        DbStatus::Loaded
    };
    s.loaded_cache = true;
}

pub fn rebuild() {
    ensure_loaded();
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);

    let (api_ids, api_names) = load_api_index();
    s.api_ids = api_ids;
    s.api_names = api_names;
    let previous_last_game_id = s.entries.iter().map(|e| e.id).max().unwrap_or(0);
    s.error_msg.clear();
    s.progress = Some((0, 0));
    s.status = DbStatus::Loading;
    s.entries.clear();
    s.entry_index.clear();

    start_job(&mut s, JobMode::Rebuild, 1, previous_last_game_id);
}

pub fn parse_names_from_hashes(include_api_named: bool, include_descriptions: bool) {
    start_name_parse(false, include_api_named, include_descriptions);
}

pub fn full_rebuild_names_from_hashes(include_api_named: bool, include_descriptions: bool) {
    start_name_parse(true, include_api_named, include_descriptions);
}

pub fn parse_game_type_names_from_hashes(link_type: encoder::LinkType) {
    start_game_type_name_parse(link_type, false);
}

pub fn full_rebuild_game_type_names_from_hashes(link_type: encoder::LinkType) {
    start_game_type_name_parse(link_type, true);
}

pub fn parse_map_names_from_hashes() {
    start_map_name_parse(false);
}

pub fn full_rebuild_map_names_from_hashes() {
    start_map_name_parse(true);
}

fn start_name_parse(
    include_already_decoded: bool,
    include_api_named: bool,
    include_descriptions: bool,
) {
    ensure_loaded();
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);
    ensure_scan_pointers(&mut s);
    let mut ids: Vec<u32> = s
        .hashes
        .values()
        .filter(|h| h.name_hash != 0 || h.upgrade_name_hash != 0)
        .filter(|h| should_decode_name_id(&s, h.id, include_already_decoded, include_api_named))
        .map(|h| h.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        s.error_msg = "No saved item hashes found. Build hashes first.".to_string();
        s.status = if s.entries.is_empty() {
            DbStatus::NotLoaded
        } else {
            DbStatus::Loaded
        };
        s.progress = None;
        return;
    }
    s.name_job = Some(NameParseJob {
        base_total: ids.len(),
        ids,
        cursor: 0,
        phase: NameParsePhase::All,
        equipment_failed_ids: Vec::new(),
        upgrade_failed_ids: Vec::new(),
        decoded: 0,
        pending_flush: 0,
        retry_counts: HashMap::new(),
        finalized_ids: HashSet::new(),
        base_finalized: 0,
        include_descriptions,
        full_rebuild: include_already_decoded,
        next_decode_at: Instant::now(),
        last_flush_at: Instant::now(),
    });
    s.name_parse_paused = false;
    s.error_msg.clear();
    s.status = DbStatus::Updating;
    s.progress = Some((0, s.name_job.as_ref().map(|j| j.base_total).unwrap_or(0)));
}

fn start_game_type_name_parse(link_type: encoder::LinkType, include_already_decoded: bool) {
    ensure_loaded();
    if link_type == encoder::LinkType::Item {
        return;
    }
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);
    ensure_scan_pointers(&mut s);

    let type_name = link_type.name().to_string();
    let mut ids: Vec<u32> = s
        .game_type_hashes
        .get(&type_name)
        .map(|m| {
            m.values()
                .filter(|h| h.name_hash != 0)
                .filter(|h| {
                    should_decode_game_type_name_id(&s, link_type, h.id, include_already_decoded)
                })
                .map(|h| h.id)
                .collect()
        })
        .unwrap_or_default();
    // Also include retryable placeholders from current game data even if hash is missing.
    if let Some(rows) = s.game_type_data.get(&type_name) {
        ids.extend(
            rows.values()
                .filter(|r| is_retryable_parse_name(&r.name))
                .map(|r| r.id),
        );
    }
    if let Some(rows) = s.game_type_names.get(&type_name) {
        ids.extend(
            rows.values()
                .filter(|r| is_retryable_parse_name(&r.name))
                .map(|r| r.id),
        );
    }
    ids.sort_unstable();
    ids.dedup();

    if ids.is_empty() {
        s.error_msg = format!(
            "No saved {} hashes found. Build hashes first.",
            link_type.name()
        );
        s.status = if s.entries.is_empty() {
            DbStatus::NotLoaded
        } else {
            DbStatus::Loaded
        };
        s.progress = None;
        return;
    }

    s.game_type_name_job = Some(GameTypeNameParseJob {
        link_type,
        base_total: ids.len(),
        ids,
        cursor: 0,
        decoded: 0,
        pending_flush: 0,
        retry_counts: HashMap::new(),
        finalized_ids: HashSet::new(),
        base_finalized: 0,
        full_rebuild: include_already_decoded,
        next_decode_at: Instant::now(),
        last_flush_at: Instant::now(),
    });
    s.error_msg.clear();
    s.status = DbStatus::Updating;
    s.progress = Some((
        0,
        s.game_type_name_job
            .as_ref()
            .map(|j| j.base_total)
            .unwrap_or(0),
    ));
}

fn start_map_name_parse(include_already_decoded: bool) {
    ensure_loaded();
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);
    ensure_scan_pointers(&mut s);

    let type_name = encoder::LinkType::Map.name().to_string();
    let mut ids_by_hash: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Some(rows) = s.game_type_data.get(&type_name) {
        for row in rows.values() {
            if row.map_name_hash == 0 {
                continue;
            }
            let needs_decode = include_already_decoded
                || row.map_name.trim().is_empty()
                || is_unresolved_decoded_text(&row.map_name);
            if needs_decode {
                ids_by_hash
                    .entry(row.map_name_hash)
                    .or_default()
                    .push(row.id);
            }
        }
    }
    let mut hashes: Vec<u32> = ids_by_hash.keys().copied().collect();
    hashes.sort_unstable();
    hashes.dedup();

    if hashes.is_empty() {
        s.error_msg = "No saved map name hashes found.".to_string();
        s.status = if s.entries.is_empty() {
            DbStatus::NotLoaded
        } else {
            DbStatus::Loaded
        };
        s.progress = None;
        return;
    }

    s.map_name_job = Some(MapNameParseJob {
        base_total: hashes.len(),
        hashes,
        ids_by_hash,
        cursor: 0,
        decoded: 0,
        pending_flush: 0,
        retry_counts: HashMap::new(),
        finalized_ids: HashSet::new(),
        base_finalized: 0,
        full_rebuild: include_already_decoded,
        next_decode_at: Instant::now(),
        last_flush_at: Instant::now(),
    });
    s.error_msg.clear();
    s.status = DbStatus::Updating;
    s.progress = Some((
        0,
        s.map_name_job.as_ref().map(|j| j.base_total).unwrap_or(0),
    ));
}

pub fn set_name_parse_paused(paused: bool) {
    let mut s = STATE.lock();
    s.name_parse_paused = paused;
}

pub fn is_name_parse_paused() -> bool {
    STATE.lock().name_parse_paused
}

pub fn start_build_missing_game_data_all_types() {
    let mut s = STATE.lock();
    s.error_msg =
        "All-type game data builds are disabled; build one selected type at a time.".to_string();
    db::log_error(&format!("{} {}", LOG_PREFIX, s.error_msg));
}

pub fn start_build_game_data_for_link_type(link_type: encoder::LinkType, full_rebuild: bool) {
    ensure_loaded();
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);
    ensure_scan_pointers(&mut s);

    let Some(content_type) = link_type_to_content_type(link_type) else {
        return;
    };

    let type_name = link_type.name().to_string();
    if full_rebuild && link_type != encoder::LinkType::Item {
        s.game_type_data.remove(&type_name);
        s.game_type_hashes.remove(&type_name);
        s.game_type_names.remove(&type_name);
    }

    let existing = s.game_type_data.get(&type_name);
    let mut tasks = Vec::<GameTypeTask>::new();
    let mut map_api_names: HashMap<u32, String> = HashMap::new();
    if link_type == encoder::LinkType::Map {
        for (id, api_name) in load_api_id_name_pairs_for_link_type(link_type) {
            map_api_names.insert(id, api_name);
        }
    } else {
        for (id, api_name) in load_api_id_name_pairs_for_link_type(link_type) {
            let already_known = if link_type == encoder::LinkType::Item {
                s.entry_index.contains_key(&id)
            } else {
                existing.map(|m| m.contains_key(&id)).unwrap_or(false)
            };
            if already_known && !full_rebuild {
                continue;
            }
            tasks.push(GameTypeTask {
                link_type,
                content_type,
                id,
                api_name,
            });
        }

        tasks.sort_by_key(|t| t.id);
        tasks.dedup_by_key(|t| t.id);

        if tasks.is_empty() {
            s.error_msg.clear();
            s.status = if s.entries.is_empty() {
                DbStatus::NotLoaded
            } else {
                DbStatus::Loaded
            };
            s.progress = None;
            return;
        }
    }

    s.game_type_job = Some(GameTypeBuildJob {
        tasks,
        cursor: 0,
        added: 0,
        pending_flush: 0,
        touched_item_cache: false,
        paused: false,
        next_step_at: Instant::now(),
        map_direct_scan: link_type == encoder::LinkType::Map,
        map_content_type: if link_type == encoder::LinkType::Map {
            content_type
        } else {
            0
        },
        map_iter_index: 0,
        map_processed: 0,
        map_total: 0,
        map_api_names,
    });
    s.status = DbStatus::Updating;
    s.progress = Some(if link_type == encoder::LinkType::Map {
        (0, 0)
    } else {
        (
            0,
            s.game_type_job.as_ref().map(|j| j.tasks.len()).unwrap_or(0),
        )
    });
    s.error_msg.clear();
}

pub fn set_build_missing_game_data_paused(paused: bool) {
    let mut s = STATE.lock();
    if let Some(job) = s.game_type_job.as_mut() {
        job.paused = paused;
    }
}

pub fn is_build_missing_game_data_paused() -> bool {
    let s = STATE.lock();
    s.game_type_job.as_ref().map(|j| j.paused).unwrap_or(false)
}

pub fn get_build_missing_game_data_progress() -> Option<(usize, usize, usize, String)> {
    let s = STATE.lock();
    let job = s.game_type_job.as_ref()?;
    let current_type = job
        .tasks
        .get(job.cursor)
        .map(|t| t.link_type.name().to_string())
        .unwrap_or_else(|| "Done".to_string());
    Some((job.cursor, job.tasks.len(), job.added, current_type))
}

pub fn queue_debug_resolve_offset(
    link_type: encoder::LinkType,
    id: u32,
    offset: usize,
) -> Result<(), String> {
    queue_debug_resolve_offset_for_content_type(link_type, id, None, offset)
}

pub fn queue_debug_resolve_offset_for_content_type(
    link_type: encoder::LinkType,
    id: u32,
    content_type_override: Option<u32>,
    offset: usize,
) -> Result<(), String> {
    ensure_loaded();
    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);
    // Replace any pending debug resolve request to keep UI responsive.
    s.debug_resolve_hash_job = None;
    s.debug_resolve_job = Some((link_type, id, content_type_override, offset));
    s.debug_resolve_result = None;
    Ok(())
}

pub fn queue_debug_resolve_hash(
    link_type: encoder::LinkType,
    id: u32,
    offset: usize,
    raw_u32: u32,
    source_ptr: Option<u64>,
) -> Result<(), String> {
    ensure_loaded();
    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);
    // Replace any pending debug resolve request to keep UI responsive.
    s.debug_resolve_job = None;
    s.debug_resolve_hash_job = Some((link_type, id, offset, raw_u32, source_ptr));
    s.debug_resolve_result = None;
    Ok(())
}

pub fn has_pending_debug_resolve() -> bool {
    let s = STATE.lock();
    s.debug_resolve_job.is_some() || s.debug_resolve_hash_job.is_some()
}

pub fn consume_debug_resolve_result() -> Option<Result<DebugResolveResult, String>> {
    STATE.lock().debug_resolve_result.take()
}

fn is_unresolved_decoded_text(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.chars()
            .all(|c| c == '?' || c == '？' || c == '\u{FFFD}' || c.is_whitespace())
}

fn is_retryable_parse_name(name: &str) -> bool {
    let t = name.trim();
    if t.is_empty() {
        return true;
    }
    if is_unresolved_decoded_text(t) {
        return true;
    }
    if is_placeholder_name(t) {
        return true;
    }
    // Generic placeholders used by game-type rows, e.g. "Wardrobe #1234", "Map #56".
    if let Some((_, tail)) = t.rsplit_once(" #") {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    t.starts_with("Category #")
}

fn is_plausible_text_hash(hash: u32) -> bool {
    (4..MAX_TEXT_HASH_VALUE).contains(&hash)
}

pub fn debug_is_known_hash_for_type(link_type: encoder::LinkType, hash: u32) -> bool {
    if hash == 0 {
        return false;
    }
    let s = STATE.lock();
    if link_type == encoder::LinkType::Item {
        return s
            .hashes
            .values()
            .any(|h| h.name_hash == hash || h.description_hash == hash);
    }
    s.game_type_hashes
        .get(link_type.name())
        .map(|m| {
            m.values()
                .any(|h| h.name_hash == hash || h.description_hash == hash)
        })
        .unwrap_or(false)
}

pub fn debug_find_resolved_types_for_id(id: u32) -> Result<Vec<u32>, String> {
    ensure_loaded();
    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);

    let prop_ctx = unsafe {
        read_prop_ctx(
            s.pointers.prop_ctx_getter,
            resolve_main_thread_id(s.pointers.main_thread_match),
        )
    }
    .ok_or_else(|| "PropContext unavailable (must be in-game on a character)".to_string())?;

    let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) }
        .ok_or_else(|| "Could not resolve ContentCtx".to_string())?;

    let (_, count_defs, iterate_content_defs) = unsafe {
        resolve_content_vfuncs(content_ctx)
            .ok_or_else(|| "Could not resolve ContentCtx vfuncs".to_string())?
    };

    let mut out = Vec::new();
    for ty in 0u32..=478u32 {
        let count = unsafe { count_defs(content_ctx, ty).max(0) as u32 };
        if count == 0 {
            continue;
        }
        let ptr = unsafe {
            find_content_ptr_by_any_u32_match(content_ctx, ty, id, iterate_content_defs, count_defs)
        };
        if !ptr.is_null() {
            out.push(ty);
        }
    }
    Ok(out)
}

fn should_decode_name_id(
    state: &State,
    id: u32,
    include_already_decoded: bool,
    include_api_named: bool,
) -> bool {
    if include_already_decoded {
        return true;
    }

    if let Some(saved_name) = state.names.get(&id).map(|n| n.name.as_str()) {
        if !is_retryable_parse_name(saved_name) {
            return false;
        }
    }

    if !include_api_named && state.api_names.contains_key(&id) {
        let current_name = state
            .entry_index
            .get(&id)
            .and_then(|idx| state.entries.get(*idx))
            .map(|e| e.name.as_str())
            .or_else(|| state.names.get(&id).map(|n| n.name.as_str()))
            .unwrap_or("");
        if !is_retryable_parse_name(current_name) {
            return false;
        }
    }

    if let Some(idx) = state.entry_index.get(&id).copied() {
        if let Some(entry) = state.entries.get(idx) {
            return is_retryable_parse_name(&entry.name);
        }
    }

    true
}

fn should_decode_game_type_name_id(
    state: &State,
    link_type: encoder::LinkType,
    id: u32,
    include_already_decoded: bool,
) -> bool {
    if include_already_decoded {
        return true;
    }
    let type_name = link_type.name();
    if let Some(saved_name) = state
        .game_type_names
        .get(type_name)
        .and_then(|m| m.get(&id))
        .map(|n| n.name.as_str())
    {
        if !is_retryable_parse_name(saved_name)
            && !is_generic_placeholder_name(saved_name, link_type)
        {
            return false;
        }
    }
    if let Some(existing_name) = state
        .game_type_data
        .get(type_name)
        .and_then(|m| m.get(&id))
        .map(|e| e.name.as_str())
    {
        if !is_retryable_parse_name(existing_name)
            && !is_generic_placeholder_name(existing_name, link_type)
        {
            return false;
        }
    }
    true
}

pub fn update() {
    ensure_loaded();
    let mut s = STATE.lock();
    if has_active_unpaused_job(&s) {
        return;
    }
    clear_paused_jobs(&mut s);

    let (api_ids, api_names) = load_api_index();
    s.api_ids = api_ids;
    s.api_names = api_names;
    s.error_msg.clear();
    s.progress = Some((0, 0));
    s.status = DbStatus::Updating;

    let last_game_id = s.entries.iter().map(|e| e.id).max().unwrap_or(0);
    start_job(&mut s, JobMode::Update, 1, last_game_id);
}

pub fn maybe_auto_update_on_load() {
    if AUTO_UPDATE_TRIGGERED.load(Ordering::Relaxed) {
        return;
    }

    let auto = { RUNTIME_CONFIG.lock().auto_update_item_db_on_load };
    if !auto {
        return;
    }

    AUTO_UPDATE_TRIGGERED.store(true, Ordering::Relaxed);
    ensure_loaded();

    let (status, count, _, _) = get_status();
    if matches!(status, DbStatus::NotLoaded) || count == 0 {
        rebuild();
    } else {
        update();
    }
}

pub fn tick() {
    let mut s = STATE.lock();
    if s.scan.is_none()
        && s.name_job.is_none()
        && s.game_type_name_job.is_none()
        && s.map_name_job.is_none()
        && s.game_type_job.is_none()
        && s.debug_resolve_job.is_none()
        && s.debug_resolve_hash_job.is_none()
    {
        return;
    }

    ensure_scan_pointers(&mut s);

    let prop_ctx = unsafe {
        read_prop_ctx(
            s.pointers.prop_ctx_getter,
            resolve_main_thread_id(s.pointers.main_thread_match),
        )
    };

    let Some(prop_ctx) = prop_ctx else {
        s.error_msg = "Waiting for PropContext (must be in-game)".to_string();
        return;
    };

    // Prioritize debug resolve jobs so UI actions are not starved by long parse jobs.
    if let Some((link_type, id, content_type_override, offset)) = s.debug_resolve_job.take() {
        let res = (|| unsafe {
            let default_content_type = link_type_to_content_type(link_type)
                .ok_or_else(|| "Unsupported link type".to_string())?;
            let content_type = content_type_override.unwrap_or(default_content_type);

            let content_ctx = read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX)
                .ok_or_else(|| "Could not resolve ContentCtx".to_string())?;

            let (get_content_by_index, count_defs, iterate_content_defs) =
                resolve_content_vfuncs(content_ctx)
                    .ok_or_else(|| "Could not resolve ContentCtx vfuncs".to_string())?;

            let api_name = get_api_name_for_link_type_id(link_type, id);
            let resolved_content_type = content_type;
            let ptr = get_strict_content_ptr_for_debug(
                &mut s,
                link_type,
                content_ctx,
                content_type,
                id,
                api_name.as_deref(),
                get_content_by_index,
                count_defs,
                iterate_content_defs,
                prop_ctx,
            );
            if ptr.is_null() {
                return Err(format!(
                    "No content ptr for type {} id {}",
                    link_type.name(),
                    id
                ));
            }

            if !is_readable(ptr, offset + std::mem::size_of::<u32>()) {
                return Err(format!("Offset 0x{:X} is not readable", offset));
            }
            let raw_u32 = (ptr.add(offset) as *const u32).read_unaligned();
            if !is_plausible_text_hash(raw_u32) {
                return Err(format!(
                    "Value at offset is not a plausible TextHash: {}",
                    raw_u32
                ));
            }

            let mut decoded_text = None;
            for _ in 0..3 {
                let attempt = decode_text_hash(&s, raw_u32, prop_ctx)
                    .map(|t| t.trim().to_string())
                    .filter(|t| !is_unresolved_decoded_text(t));
                if attempt.is_some() {
                    decoded_text = attempt;
                    break;
                }
            }

            let coded_text = {
                let coded_ptr = resolve_coded_text_ptr(&s, raw_u32, prop_ctx);
                if coded_ptr.is_null() {
                    String::new()
                } else {
                    read_wide_string(coded_ptr, 1024).unwrap_or_default()
                }
            };

            if decoded_text.is_none() && coded_text.is_empty() {
                return Err("resolve_text_hash/decode_text failed".to_string());
            }
            Ok(DebugResolveResult {
                link_type,
                id,
                resolved_content_type,
                offset,
                raw_u32,
                coded_text,
                decoded_text,
            })
        })();

        s.debug_resolve_result = Some(res);
        return;
    }

    if let Some((link_type, id, offset, raw_u32, source_ptr)) = s.debug_resolve_hash_job.take() {
        let res = (|| unsafe {
            if !is_plausible_text_hash(raw_u32) {
                return Err(format!(
                    "Selected value is not a plausible TextHash: {}",
                    raw_u32
                ));
            }

            let mut decoded_text = None;
            for _ in 0..3 {
                let attempt = decode_text_hash(&s, raw_u32, prop_ctx)
                    .map(|t| t.trim().to_string())
                    .filter(|t| !is_unresolved_decoded_text(t));
                if attempt.is_some() {
                    decoded_text = attempt;
                    break;
                }
            }

            let coded_text = {
                let coded_ptr = resolve_coded_text_ptr(&s, raw_u32, prop_ctx);
                if coded_ptr.is_null() {
                    String::new()
                } else {
                    read_wide_string(coded_ptr, 1024).unwrap_or_default()
                }
            };

            let mut coded_text = coded_text;
            // Subdef fallback: TextHash(0x60/0x70) has adjacent CodedText pointer at -0x8.
            if decoded_text.is_none() && coded_text.is_empty() {
                if let Some(src) = source_ptr {
                    let src_ptr = src as usize as *const u8;
                    if !src_ptr.is_null() && offset >= 8 {
                        let coded_off = offset - 8;
                        if let Some(coded_ptr) = read_ptr(src_ptr, coded_off) {
                            if !coded_ptr.is_null() {
                                if let Some(text) = read_wide_string(coded_ptr as *const u16, 1024)
                                {
                                    let t = text.trim().to_string();
                                    if !t.is_empty() {
                                        coded_text = t.clone();
                                        if !is_unresolved_decoded_text(&t) {
                                            decoded_text = Some(t);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if decoded_text.is_none() && coded_text.is_empty() {
                return Err(
                    "resolve_text_hash/decode_text failed (including coded-text fallback)"
                        .to_string(),
                );
            }

            Ok(DebugResolveResult {
                link_type,
                id,
                resolved_content_type: 0,
                offset,
                raw_u32,
                coded_text,
                decoded_text,
            })
        })();

        s.debug_resolve_result = Some(res);
        return;
    }

    if let Some(mut job) = s.scan.take() {
        let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) };
        let Some(content_ctx) = content_ctx else {
            s.error_msg = "Could not resolve ContentCtx".to_string();
            return;
        };

        let (get_content_by_index, _, _) = unsafe {
            match resolve_content_vfuncs(content_ctx) {
                Some(v) => v,
                None => {
                    s.error_msg = "Could not resolve ContentCtx vfuncs".to_string();
                    return;
                }
            }
        };

        let scan_started = Instant::now();
        for _ in 0..ITEM_SCAN_PER_TICK {
            if job.current_id > job.end_id {
                break;
            }
            if scan_started.elapsed() >= Duration::from_millis(ITEM_SCAN_BUDGET_MS) {
                break;
            }

            let id = job.current_id;
            job.current_id = job.current_id.saturating_add(1);
            job.processed = job.processed.saturating_add(1);
            let item_ptr = unsafe { get_content_by_index(content_ctx, ITEM_CONTENT_TYPE, id) };
            if item_ptr.is_null() {
                continue;
            }
            if let Some((
                entry,
                content_index,
                name_hash,
                description_hash,
                upgrade_name_hash,
                base_upgrade_item_id,
            )) = unsafe { read_item_entry(&s, item_ptr, id) }
            {
                job.last_found_id = job.last_found_id.max(entry.id);
                job.end_id = job
                    .end_id
                    .max(job.last_found_id.saturating_add(job.trailing_gap));
                if matches!(job.mode, JobMode::Rebuild) || !s.entry_index.contains_key(&entry.id) {
                    s.upsert_entry(
                        entry,
                        content_index,
                        name_hash,
                        description_hash,
                        upgrade_name_hash,
                        base_upgrade_item_id,
                    );
                    job.added = job.added.saturating_add(1);
                }
            }
        }

        let done = job.current_id > job.end_id;

        let total = job.end_id.saturating_sub(job.start_id).saturating_add(1) as usize;
        s.progress = Some((job.processed.min(total), total));

        if done {
            finalize_job(&mut s, job);
        } else {
            s.scan = Some(job);
        }
        return;
    }

    if let Some(mut job) = s.name_job.take() {
        if s.name_parse_paused {
            s.progress = Some((job.base_finalized, job.base_total.max(1)));
            s.name_job = Some(job);
            s.status = DbStatus::Updating;
            return;
        }

        let total = job.base_total;
        let now = Instant::now();
        if now < job.next_decode_at {
            s.progress = Some((job.base_finalized, total.max(1)));
            s.name_job = Some(job);
            s.status = DbStatus::Updating;
            return;
        }

        let item_content_access = unsafe {
            read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX).and_then(|content_ctx| {
                resolve_content_vfuncs(content_ctx)
                    .map(|(get_content_by_index, _, _)| (content_ctx, get_content_by_index))
            })
        };

        let mut steps = 0usize;
        let parse_started = Instant::now();
        while job.cursor < job.ids.len() && steps < DECODES_PER_TICK.max(1) {
            if parse_started.elapsed() >= Duration::from_millis(NAME_PARSE_BUDGET_MS) {
                break;
            }
            let id = job.ids[job.cursor];
            job.cursor += 1;
            steps += 1;
            if job.finalized_ids.contains(&id) {
                continue;
            }
            let name_hash = s.hashes.get(&id).map(|h| h.name_hash).unwrap_or(0);
            let upgrade_name_hash = s.hashes.get(&id).map(|h| h.upgrade_name_hash).unwrap_or(0);
            let item_type_code = s
                .entry_index
                .get(&id)
                .and_then(|&idx| s.entries.get(idx))
                .map(|e| e.item_type_code)
                .unwrap_or(u32::MAX);
            let can_use_skin_fallback = matches!(item_type_code, 0 | 24); // Armor | Weapon
            let is_upgrade_item = item_type_code == 23; // UpgradeComponent

            if let Some((content_ctx, get_content_by_index)) = item_content_access {
                let fallback_skin_id = s
                    .entry_index
                    .get(&id)
                    .and_then(|&idx| s.entries.get(idx))
                    .map(|e| e.default_skin_id)
                    .unwrap_or(0);
                let fallback_base_upgrade = s
                    .hashes
                    .get(&id)
                    .map(|h| h.base_upgrade_item_id)
                    .unwrap_or(0);
                if fallback_skin_id == 0 || fallback_base_upgrade == 0 {
                    unsafe {
                        refresh_item_fallback_ids(&mut s, id, content_ctx, get_content_by_index)
                    };
                }
            }
            let decoded = if name_hash != 0 {
                if let Some(cached) = s.decoded_text_by_hash.get(&name_hash).cloned() {
                    Some(cached)
                } else {
                    let out = unsafe {
                        decode_text_hash(&s, name_hash, prop_ctx)
                            .or_else(|| resolve_text_hash_coded_only(&s, name_hash, prop_ctx))
                    };
                    if let Some(ref text) = out {
                        if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                            s.decoded_text_by_hash.insert(name_hash, text.clone());
                        }
                    }
                    out
                }
            } else {
                None
            };
            let mut name = decoded
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty() && !is_unresolved_decoded_text(n));
            if name.is_none() && can_use_skin_fallback && job.phase == NameParsePhase::Equipment {
                let default_skin_id = s
                    .entry_index
                    .get(&id)
                    .and_then(|&idx| s.entries.get(idx))
                    .map(|e| e.default_skin_id)
                    .unwrap_or(0);
                name = unsafe { resolve_item_name_fallback_from_skin(&mut s, id, default_skin_id) };
            }

            let upgrade_name_fallback_for_name =
                if name.is_none() && is_upgrade_item && job.phase == NameParsePhase::Upgrade {
                    unsafe {
                        resolve_upgrade_name_fallback_from_base_upgrade_item(
                            &mut s,
                            id,
                            item_content_access,
                        )
                    }
                } else {
                    None
                };

            if name.is_none() {
                name = upgrade_name_fallback_for_name;
            }

            if name
                .as_ref()
                .map(|n| is_unresolved_decoded_text(n))
                .unwrap_or(false)
            {
                name = None;
            }

            if name.is_none() && job.phase == NameParsePhase::All {
                let mut deferred = false;
                if can_use_skin_fallback {
                    job.equipment_failed_ids.push(id);
                    deferred = true;
                }
                if is_upgrade_item {
                    job.upgrade_failed_ids.push(id);
                    deferred = true;
                }
                if deferred {
                    continue;
                }
            }

            if name.is_none() {
                let tries = job.retry_counts.entry(id).or_insert(0);
                if *tries < NAME_DECODE_MAX_RETRIES {
                    *tries += 1;
                    // Retry immediately instead of queueing at the tail, so progress does not
                    // appear stalled at 0 on large datasets.
                    job.ids.insert(job.cursor, id);
                    continue;
                }
            }

            let name = name.unwrap_or_else(|| format!("Item #{}", id));

            let ts = now_unix();
            s.names.insert(
                id,
                ItemNameEntry {
                    id,
                    name: name.clone(),
                    last_seen_unix: ts,
                },
            );
            if let Some(idx) = s.entry_index.get(&id).copied() {
                let decoded_desc = if job.include_descriptions {
                    let description_hash =
                        s.hashes.get(&id).map(|h| h.description_hash).unwrap_or(0);
                    let has_existing_desc = s
                        .entries
                        .get(idx)
                        .map(|e| !e.description.trim().is_empty())
                        .unwrap_or(false);
                    let should_decode_desc =
                        description_hash != 0 && (job.full_rebuild || !has_existing_desc);
                    if should_decode_desc {
                        let decoded_desc = if let Some(cached) =
                            s.decoded_text_by_hash.get(&description_hash).cloned()
                        {
                            Some(cached)
                        } else {
                            let out = unsafe {
                                decode_text_hash(&s, description_hash, prop_ctx).or_else(|| {
                                    resolve_text_hash_coded_only(&s, description_hash, prop_ctx)
                                })
                            };
                            if let Some(ref text) = out {
                                if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                                    s.decoded_text_by_hash
                                        .insert(description_hash, text.clone());
                                }
                            }
                            out
                        };
                        decoded_desc.filter(|d| !d.trim().is_empty())
                    } else {
                        None
                    }
                } else {
                    None
                };
                let decoded_upgrade_name = if upgrade_name_hash != 0 {
                    if let Some(cached) = s.decoded_text_by_hash.get(&upgrade_name_hash).cloned() {
                        Some(cached)
                    } else {
                        let out = unsafe {
                            decode_text_hash(&s, upgrade_name_hash, prop_ctx).or_else(|| {
                                resolve_text_hash_coded_only(&s, upgrade_name_hash, prop_ctx)
                            })
                        };
                        if let Some(ref text) = out {
                            if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                                s.decoded_text_by_hash
                                    .insert(upgrade_name_hash, text.clone());
                            }
                        }
                        out
                    }
                } else {
                    None
                };
                let fallback_upgrade_name = if is_upgrade_item
                    && job.phase == NameParsePhase::Upgrade
                    && decoded_upgrade_name
                        .as_ref()
                        .map(|u| u.trim().is_empty() || is_unresolved_decoded_text(u))
                        .unwrap_or(true)
                {
                    unsafe {
                        resolve_upgrade_name_fallback_from_base_upgrade_item(
                            &mut s,
                            id,
                            item_content_access,
                        )
                    }
                } else {
                    None
                };

                if let Some(entry) = s.entries.get_mut(idx) {
                    entry.name = name.clone();
                    if is_upgrade_item {
                        if let Some(upgrade_name) =
                            decoded_upgrade_name.filter(|u| !u.trim().is_empty())
                        {
                            entry.upgrade_name = upgrade_name;
                        } else if let Some(upgrade_name) = fallback_upgrade_name {
                            entry.upgrade_name = upgrade_name;
                        } else {
                            entry.upgrade_name.clear();
                        }
                    } else {
                        entry.upgrade_name.clear();
                    }
                    if let Some(desc) = decoded_desc {
                        entry.description = desc;
                    }
                }
            }
            job.decoded = job.decoded.saturating_add(1);
            job.pending_flush = job.pending_flush.saturating_add(1);
            job.retry_counts.remove(&id);
            s.name_failed.remove(&id);
            if job.finalized_ids.insert(id) {
                job.base_finalized = job.base_finalized.saturating_add(1);
            }

            // Persist in checkpoints to reduce render-thread I/O stutter.
            let flush_due =
                job.last_flush_at.elapsed() >= Duration::from_millis(NAME_SAVE_INTERVAL_MS);
            if job.pending_flush >= NAME_SAVE_EVERY || flush_due {
                save_names_cache(&s.names);
                save_failed_names_cache(&s.name_failed);
                job.pending_flush = 0;
                job.last_flush_at = Instant::now();
            }
        }
        job.next_decode_at = Instant::now() + Duration::from_millis(NAME_DECODE_COOLDOWN_MS);

        if job.cursor >= job.ids.len() {
            loop {
                match job.phase {
                    NameParsePhase::All => {
                        let mut seen = HashSet::new();
                        let next_ids: Vec<u32> = job
                            .equipment_failed_ids
                            .iter()
                            .copied()
                            .filter(|id| !job.finalized_ids.contains(id))
                            .filter(|id| seen.insert(*id))
                            .collect();
                        job.equipment_failed_ids.clear();
                        job.phase = NameParsePhase::Equipment;
                        job.ids = next_ids;
                        job.cursor = 0;
                        job.retry_counts.clear();
                        if !job.ids.is_empty() {
                            break;
                        }
                    }
                    NameParsePhase::Equipment => {
                        let mut seen = HashSet::new();
                        let next_ids: Vec<u32> = job
                            .upgrade_failed_ids
                            .iter()
                            .copied()
                            .filter(|id| !job.finalized_ids.contains(id))
                            .filter(|id| seen.insert(*id))
                            .collect();
                        job.upgrade_failed_ids.clear();
                        job.phase = NameParsePhase::Upgrade;
                        job.ids = next_ids;
                        job.cursor = 0;
                        job.retry_counts.clear();
                        if !job.ids.is_empty() {
                            break;
                        }
                    }
                    NameParsePhase::Upgrade => break,
                }
            }
        }

        s.progress = Some((job.base_finalized, total.max(1)));
        if job.base_finalized >= total {
            save_cache(CACHE_FILE, &s.entries);
            save_names_cache(&s.names);
            save_failed_names_cache(&s.name_failed);
            s.name_job = None;
            s.status = DbStatus::Loaded;
            s.progress = None;
            s.error_msg.clear();
            db::log_debug(&format!(
                "{} name parse completed: {} / {} decoded",
                LOG_PREFIX, job.decoded, total
            ));
        } else {
            s.name_job = Some(job);
            s.status = DbStatus::Updating;
        }
    }

    if let Some(mut job) = s.game_type_name_job.take() {
        let total = job.base_total;
        let now = Instant::now();
        if now < job.next_decode_at {
            s.progress = Some((job.base_finalized, total.max(1)));
            s.game_type_name_job = Some(job);
            s.status = DbStatus::Updating;
            return;
        }

        let type_name = job.link_type.name().to_string();
        let game_type_content_access = unsafe {
            read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX).and_then(|content_ctx| {
                resolve_content_vfuncs(content_ctx).map(
                    |(get_content_by_index, count_defs, iterate_content_defs)| {
                        (
                            content_ctx,
                            get_content_by_index,
                            count_defs,
                            iterate_content_defs,
                        )
                    },
                )
            })
        };
        let game_type_content_type = link_type_to_content_type(job.link_type).unwrap_or(0);
        let mut steps = 0usize;
        let parse_started = Instant::now();
        while job.cursor < job.ids.len() && steps < DECODES_PER_TICK.max(1) {
            if parse_started.elapsed() >= Duration::from_millis(NAME_PARSE_BUDGET_MS) {
                break;
            }
            let id = job.ids[job.cursor];
            job.cursor += 1;
            steps += 1;
            if job.finalized_ids.contains(&id) {
                continue;
            }

            let mut name_hash = s
                .game_type_hashes
                .get(&type_name)
                .and_then(|m| m.get(&id))
                .map(|h| h.name_hash)
                .unwrap_or(0);
            if name_hash == 0 && game_type_content_type != 0 {
                if let Some((content_ctx, get_content_by_index, count_defs, iterate_content_defs)) =
                    game_type_content_access
                {
                    let api_name =
                        get_api_name_for_link_type_id(job.link_type, id).unwrap_or_default();
                    let ptr = unsafe {
                        get_content_ptr_for_type_id(
                            &mut s,
                            job.link_type,
                            content_ctx,
                            game_type_content_type,
                            id,
                            if api_name.is_empty() {
                                None
                            } else {
                                Some(&api_name)
                            },
                            get_content_by_index,
                            count_defs,
                            iterate_content_defs,
                            prop_ctx,
                            false,
                        )
                    };
                    if !ptr.is_null() {
                        let refreshed_hash = unsafe {
                            read_non_item_name_hash(
                                &mut s,
                                job.link_type,
                                game_type_content_type,
                                content_ctx,
                                get_content_by_index,
                                count_defs,
                                iterate_content_defs,
                                ptr,
                                &api_name,
                                prop_ctx,
                            )
                        };
                        if refreshed_hash != 0 {
                            name_hash = refreshed_hash;
                            let ts = now_unix();
                            s.game_type_hashes
                                .entry(type_name.clone())
                                .or_default()
                                .entry(id)
                                .and_modify(|h| {
                                    h.name_hash = refreshed_hash;
                                    h.last_seen_unix = ts;
                                })
                                .or_insert(ItemHashEntry {
                                    id,
                                    content_index: id,
                                    name_hash: refreshed_hash,
                                    description_hash: 0,
                                    upgrade_name_hash: 0,
                                    base_upgrade_item_id: 0,
                                    last_seen_unix: ts,
                                });
                        }
                    }
                }
            }

            let decoded = if name_hash != 0 {
                if let Some(cached) = s.decoded_text_by_hash.get(&name_hash).cloned() {
                    Some(cached)
                } else if job.link_type == encoder::LinkType::Wardrobe {
                    let out = unsafe { resolve_text_hash_coded_only(&s, name_hash, prop_ctx) };
                    if let Some(ref text) = out {
                        if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                            s.decoded_text_by_hash.insert(name_hash, text.clone());
                        }
                    }
                    out
                } else {
                    let out = unsafe {
                        decode_text_hash(&s, name_hash, prop_ctx)
                            .or_else(|| resolve_text_hash_coded_only(&s, name_hash, prop_ctx))
                    };
                    if let Some(ref text) = out {
                        if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                            s.decoded_text_by_hash.insert(name_hash, text.clone());
                        }
                    }
                    out
                }
            } else {
                None
            };

            let is_wardrobe = job.link_type == encoder::LinkType::Wardrobe;
            let max_retries = if is_wardrobe {
                WARDROBE_NAME_DECODE_MAX_RETRIES
            } else {
                GAME_TYPE_NAME_DECODE_MAX_RETRIES
            };

            let Some(name_raw) = decoded else {
                let tries = job.retry_counts.entry(id).or_insert(0);
                if *tries < max_retries {
                    *tries += 1;
                    job.ids.insert(job.cursor, id);
                } else if job.finalized_ids.insert(id) {
                    let placeholder = format!("{} #{}", type_name, id);
                    if let Some(row) = s
                        .game_type_data
                        .get_mut(&type_name)
                        .and_then(|m| m.get_mut(&id))
                    {
                        row.name = placeholder.clone();
                    }
                    if let Some(names) = s.game_type_names.get_mut(&type_name) {
                        names.remove(&id);
                    }
                    job.base_finalized = job.base_finalized.saturating_add(1);
                }
                continue;
            };

            let name = name_raw.trim().to_string();
            if name.is_empty() || is_unresolved_decoded_text(&name) {
                let tries = job.retry_counts.entry(id).or_insert(0);
                if *tries < max_retries {
                    *tries += 1;
                    job.ids.insert(job.cursor, id);
                } else if job.finalized_ids.insert(id) {
                    let placeholder = format!("{} #{}", type_name, id);
                    if let Some(row) = s
                        .game_type_data
                        .get_mut(&type_name)
                        .and_then(|m| m.get_mut(&id))
                    {
                        row.name = placeholder.clone();
                    }
                    if let Some(names) = s.game_type_names.get_mut(&type_name) {
                        names.remove(&id);
                    }
                    job.base_finalized = job.base_finalized.saturating_add(1);
                }
                continue;
            }

            let ts = now_unix();
            s.game_type_names
                .entry(type_name.clone())
                .or_default()
                .insert(
                    id,
                    ItemNameEntry {
                        id,
                        name: name.clone(),
                        last_seen_unix: ts,
                    },
                );
            if let Some(row) = s
                .game_type_data
                .entry(type_name.clone())
                .or_default()
                .get_mut(&id)
            {
                row.name = name.clone();
                row.last_seen_unix = ts;
            }

            job.decoded = job.decoded.saturating_add(1);
            job.pending_flush = job.pending_flush.saturating_add(1);
            if job.finalized_ids.insert(id) {
                job.base_finalized = job.base_finalized.saturating_add(1);
            }

            let flush_due =
                job.last_flush_at.elapsed() >= Duration::from_millis(NAME_SAVE_INTERVAL_MS);
            if job.pending_flush >= NAME_SAVE_EVERY || flush_due {
                save_game_type_data_cache(
                    &s.game_type_data,
                    &s.game_type_hashes,
                    &s.game_type_names,
                );
                job.pending_flush = 0;
                job.last_flush_at = Instant::now();
            }
        }

        job.next_decode_at = Instant::now() + Duration::from_millis(NAME_DECODE_COOLDOWN_MS);
        s.progress = Some((job.base_finalized, total.max(1)));
        if job.base_finalized >= total {
            save_game_type_data_cache(&s.game_type_data, &s.game_type_hashes, &s.game_type_names);
            s.game_type_name_job = None;
            s.status = if s.entries.is_empty() {
                DbStatus::NotLoaded
            } else {
                DbStatus::Loaded
            };
            s.progress = None;
            s.error_msg.clear();
            db::log_debug(&format!(
                "{} {} name parse completed: {} / {} decoded (full_rebuild={})",
                LOG_PREFIX,
                job.link_type.name(),
                job.decoded,
                total,
                job.full_rebuild
            ));
        } else {
            s.game_type_name_job = Some(job);
            s.status = DbStatus::Updating;
        }
    }

    if let Some(mut job) = s.map_name_job.take() {
        let total = job.base_total;
        let now = Instant::now();
        if now < job.next_decode_at {
            s.progress = Some((job.base_finalized, total.max(1)));
            s.map_name_job = Some(job);
            s.status = DbStatus::Updating;
            return;
        }
        let type_name = encoder::LinkType::Map.name().to_string();
        let mut steps = 0usize;
        let parse_started = Instant::now();
        while job.cursor < job.hashes.len() && steps < DECODES_PER_TICK.max(1) {
            if parse_started.elapsed() >= Duration::from_millis(NAME_PARSE_BUDGET_MS) {
                break;
            }
            let map_name_hash = job.hashes[job.cursor];
            job.cursor += 1;
            steps += 1;
            if job.finalized_ids.contains(&map_name_hash) {
                continue;
            }
            let decoded = if map_name_hash != 0 {
                if let Some(cached) = s.decoded_text_by_hash.get(&map_name_hash).cloned() {
                    Some(cached)
                } else {
                    let out = unsafe {
                        decode_text_hash(&s, map_name_hash, prop_ctx)
                            .or_else(|| resolve_text_hash_coded_only(&s, map_name_hash, prop_ctx))
                    };
                    if let Some(ref text) = out {
                        if !text.trim().is_empty() && !is_unresolved_decoded_text(text) {
                            s.decoded_text_by_hash.insert(map_name_hash, text.clone());
                        }
                    }
                    out
                }
            } else {
                None
            };
            let Some(name_raw) = decoded else {
                let tries = job.retry_counts.entry(map_name_hash).or_insert(0);
                if *tries < GAME_TYPE_NAME_DECODE_MAX_RETRIES {
                    *tries += 1;
                    job.hashes.push(map_name_hash);
                } else if job.finalized_ids.insert(map_name_hash) {
                    job.base_finalized = job.base_finalized.saturating_add(1);
                }
                continue;
            };
            let name = name_raw.trim().to_string();
            if name.is_empty() || is_unresolved_decoded_text(&name) {
                let tries = job.retry_counts.entry(map_name_hash).or_insert(0);
                if *tries < GAME_TYPE_NAME_DECODE_MAX_RETRIES {
                    *tries += 1;
                    job.hashes.push(map_name_hash);
                } else if job.finalized_ids.insert(map_name_hash) {
                    job.base_finalized = job.base_finalized.saturating_add(1);
                }
                continue;
            }
            let mut updated_count = 0usize;
            if let Some(ids) = job.ids_by_hash.get(&map_name_hash) {
                for &id in ids {
                    if let Some(row) = s
                        .game_type_data
                        .entry(type_name.clone())
                        .or_default()
                        .get_mut(&id)
                    {
                        row.map_name = name.clone();
                        row.last_seen_unix = now_unix();
                        updated_count += 1;
                    }
                }
            }
            job.decoded = job.decoded.saturating_add(1);
            job.pending_flush = job.pending_flush.saturating_add(updated_count.max(1));
            if job.finalized_ids.insert(map_name_hash) {
                job.base_finalized = job.base_finalized.saturating_add(1);
            }
            let flush_due =
                job.last_flush_at.elapsed() >= Duration::from_millis(NAME_SAVE_INTERVAL_MS);
            if job.pending_flush >= NAME_SAVE_EVERY || flush_due {
                save_game_type_data_cache(
                    &s.game_type_data,
                    &s.game_type_hashes,
                    &s.game_type_names,
                );
                job.pending_flush = 0;
                job.last_flush_at = Instant::now();
            }
        }
        job.next_decode_at = Instant::now() + Duration::from_millis(NAME_DECODE_COOLDOWN_MS);
        s.progress = Some((job.base_finalized, total.max(1)));
        if job.base_finalized >= total {
            save_game_type_data_cache(&s.game_type_data, &s.game_type_hashes, &s.game_type_names);
            s.map_name_job = None;
            s.status = if s.entries.is_empty() {
                DbStatus::NotLoaded
            } else {
                DbStatus::Loaded
            };
            s.progress = None;
            s.error_msg.clear();
            db::log_debug(&format!(
                "{} map name parse completed: {} / {} decoded (full_rebuild={})",
                LOG_PREFIX, job.decoded, total, job.full_rebuild
            ));
        } else {
            s.map_name_job = Some(job);
            s.status = DbStatus::Updating;
        }
    }

    if let Some(mut job) = s.game_type_job.take() {
        if job.paused {
            s.status = DbStatus::Updating;
            s.progress = Some((job.cursor, job.tasks.len()));
            s.game_type_job = Some(job);
            return;
        }
        let now = Instant::now();
        if now < job.next_step_at {
            s.status = DbStatus::Updating;
            s.progress = Some((job.cursor, job.tasks.len()));
            s.game_type_job = Some(job);
            return;
        }

        let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) };
        let Some(content_ctx) = content_ctx else {
            s.error_msg = "Could not resolve ContentCtx".to_string();
            s.game_type_job = Some(job);
            return;
        };

        let (get_content_by_index, count_defs, iterate_content_defs) = unsafe {
            match resolve_content_vfuncs(content_ctx) {
                Some(v) => v,
                None => {
                    s.error_msg = "Could not resolve ContentCtx vfuncs".to_string();
                    s.game_type_job = Some(job);
                    return;
                }
            }
        };

        if job.map_direct_scan {
            if job.map_total == 0 {
                job.map_total =
                    unsafe { count_defs(content_ctx, job.map_content_type) }.max(0) as usize;
            }
            let per_tick_limit = GAME_DATA_PER_TICK.max(16);
            let mut processed = 0usize;
            while processed < per_tick_limit {
                let ptr = unsafe {
                    iterate_content_defs(
                        content_ctx,
                        job.map_content_type,
                        &mut job.map_iter_index as *mut u32,
                    )
                };
                if ptr.is_null() {
                    job.map_iter_index = u32::MAX;
                    break;
                }
                processed = processed.saturating_add(1);
                job.map_processed = job.map_processed.saturating_add(1);
                if !unsafe { is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) } {
                    continue;
                }
                let c_type = unsafe { (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned() };
                if c_type != job.map_content_type {
                    continue;
                }
                if !unsafe { is_readable(ptr, POI_DEF_ID + std::mem::size_of::<u32>()) } {
                    continue;
                }
                let id = unsafe { (ptr.add(POI_DEF_ID) as *const u32).read_unaligned() };
                if id == 0 {
                    continue;
                }
                let type_name = encoder::LinkType::Map.name().to_string();
                let exists = s
                    .game_type_data
                    .get(&type_name)
                    .map(|b| b.contains_key(&id))
                    .unwrap_or(false);
                if exists {
                    continue;
                }

                let name_hash = unsafe {
                    read_non_item_name_hash(
                        &mut s,
                        encoder::LinkType::Map,
                        job.map_content_type,
                        content_ctx,
                        get_content_by_index,
                        count_defs,
                        iterate_content_defs,
                        ptr,
                        "",
                        prop_ctx,
                    )
                };
                let (poi_type_code, map_name_hash, map_name) =
                    unsafe { read_poi_metadata(&mut s, ptr, id, prop_ctx) }.unwrap_or((
                        0,
                        0,
                        String::new(),
                    ));
                let resolved_name = s
                    .game_type_names
                    .get(&type_name)
                    .and_then(|b| b.get(&id))
                    .map(|e| e.name.clone())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("{} #{}", encoder::LinkType::Map.name(), id));
                let in_api = job.map_api_names.contains_key(&id);
                let ts = now_unix();
                s.game_type_data
                    .entry(type_name.clone())
                    .or_default()
                    .insert(
                        id,
                        GameTypeDataEntry {
                            link_type: type_name.clone(),
                            id,
                            name: resolved_name.clone(),
                            in_api,
                            name_hash,
                            description_hash: 0,
                            description: String::new(),
                            skin_rarity_code: 0,
                            skin_flags_code: 0,
                            skin_type_code: 0,
                            poi_type_code,
                            map_name_hash,
                            map_name,
                            last_seen_unix: ts,
                        },
                    );
                s.game_type_hashes
                    .entry(type_name.clone())
                    .or_default()
                    .insert(
                        id,
                        ItemHashEntry {
                            id,
                            content_index: id,
                            name_hash,
                            description_hash: 0,
                            upgrade_name_hash: 0,
                            base_upgrade_item_id: 0,
                            last_seen_unix: ts,
                        },
                    );
                s.game_type_names.entry(type_name).or_default().insert(
                    id,
                    ItemNameEntry {
                        id,
                        name: resolved_name,
                        last_seen_unix: ts,
                    },
                );
                job.added = job.added.saturating_add(1);
                job.pending_flush = job.pending_flush.saturating_add(1);

                if job.pending_flush >= GAME_DATA_SAVE_EVERY {
                    save_game_type_data_cache(
                        &s.game_type_data,
                        &s.game_type_hashes,
                        &s.game_type_names,
                    );
                    job.pending_flush = 0;
                }
            }

            job.next_step_at = Instant::now() + Duration::from_millis(MAP_GAME_DATA_COOLDOWN_MS);

            let done = job.map_iter_index == u32::MAX;
            s.status = DbStatus::Updating;
            s.progress = Some((job.map_processed, job.map_total.max(job.map_processed)));
            if done {
                save_game_type_data_cache(
                    &s.game_type_data,
                    &s.game_type_hashes,
                    &s.game_type_names,
                );
                s.game_type_job = None;
                s.status = if s.entries.is_empty() {
                    DbStatus::NotLoaded
                } else {
                    DbStatus::Loaded
                };
                s.progress = None;
                s.error_msg.clear();
            } else {
                s.game_type_job = Some(job);
            }
            return;
        }

        let per_tick_limit = job
            .tasks
            .get(job.cursor)
            .map(|t| match t.link_type {
                encoder::LinkType::Map => MAP_GAME_DATA_PER_TICK,
                encoder::LinkType::Wardrobe => WARDROBE_GAME_DATA_PER_TICK,
                _ => GAME_DATA_PER_TICK,
            })
            .unwrap_or(GAME_DATA_PER_TICK);
        let mut processed = 0usize;
        while job.cursor < job.tasks.len() && processed < per_tick_limit {
            let task = &job.tasks[job.cursor];
            let ts = now_unix();

            if task.link_type == encoder::LinkType::Item {
                if !s.entry_index.contains_key(&task.id) {
                    let ptr = unsafe {
                        get_content_ptr_for_type_id(
                            &mut s,
                            task.link_type,
                            content_ctx,
                            task.content_type,
                            task.id,
                            Some(&task.api_name),
                            get_content_by_index,
                            count_defs,
                            iterate_content_defs,
                            prop_ctx,
                            false,
                        )
                    };
                    if !ptr.is_null() {
                        if let Some((
                            entry,
                            content_index,
                            name_hash,
                            description_hash,
                            upgrade_name_hash,
                            base_upgrade_item_id,
                        )) = unsafe { read_item_entry(&s, ptr, task.id) }
                        {
                            s.upsert_entry(
                                entry,
                                content_index,
                                name_hash,
                                description_hash,
                                upgrade_name_hash,
                                base_upgrade_item_id,
                            );
                            job.added = job.added.saturating_add(1);
                            job.pending_flush = job.pending_flush.saturating_add(1);
                            job.touched_item_cache = true;
                        }
                    }
                }
            } else {
                let type_name = task.link_type.name().to_string();
                let exists = s
                    .game_type_data
                    .get(&type_name)
                    .map(|b| b.contains_key(&task.id))
                    .unwrap_or(false);
                if !exists {
                    let ptr = unsafe {
                        get_content_ptr_for_type_id(
                            &mut s,
                            task.link_type,
                            content_ctx,
                            task.content_type,
                            task.id,
                            Some(&task.api_name),
                            get_content_by_index,
                            count_defs,
                            iterate_content_defs,
                            prop_ctx,
                            false,
                        )
                    };
                    if !ptr.is_null() {
                        let name_hash = unsafe {
                            read_non_item_name_hash(
                                &mut s,
                                task.link_type,
                                task.content_type,
                                content_ctx,
                                get_content_by_index,
                                count_defs,
                                iterate_content_defs,
                                ptr,
                                &task.api_name,
                                prop_ctx,
                            )
                        };
                        let mut resolved_name = String::new();
                        if resolved_name.trim().is_empty() {
                            if let Some(prev) = s
                                .game_type_names
                                .get(&type_name)
                                .and_then(|b| b.get(&task.id))
                                .map(|e| e.name.clone())
                            {
                                if !prev.trim().is_empty() && !is_unresolved_decoded_text(&prev) {
                                    resolved_name = prev;
                                }
                            }
                        }
                        if resolved_name.trim().is_empty()
                            || is_unresolved_decoded_text(&resolved_name)
                        {
                            resolved_name = format!("{} #{}", task.link_type.name(), task.id);
                        }
                        let (skin_rarity_code, skin_flags_code, skin_type_code) =
                            if task.link_type == encoder::LinkType::Wardrobe {
                                unsafe { read_skin_metadata(ptr, task.id) }.unwrap_or((0, 0, 0))
                            } else {
                                (0, 0, 0)
                            };
                        let (poi_type_code, map_name_hash, map_name) =
                            if task.link_type == encoder::LinkType::Map {
                                unsafe { read_poi_metadata(&mut s, ptr, task.id, prop_ctx) }
                                    .unwrap_or((0, 0, String::new()))
                            } else {
                                (0, 0, String::new())
                            };
                        let description_hash = 0;
                        let decoded_desc = String::new();
                        let bucket = s.game_type_data.entry(type_name.clone()).or_default();
                        bucket.insert(
                            task.id,
                            GameTypeDataEntry {
                                link_type: type_name.clone(),
                                id: task.id,
                                name: resolved_name.clone(),
                                in_api: true,
                                name_hash,
                                description_hash,
                                description: decoded_desc,
                                skin_rarity_code,
                                skin_flags_code,
                                skin_type_code,
                                poi_type_code,
                                map_name_hash,
                                map_name,
                                last_seen_unix: ts,
                            },
                        );
                        s.game_type_hashes
                            .entry(type_name.clone())
                            .or_default()
                            .insert(
                                task.id,
                                ItemHashEntry {
                                    id: task.id,
                                    content_index: task.id,
                                    name_hash,
                                    description_hash,
                                    upgrade_name_hash: 0,
                                    base_upgrade_item_id: 0,
                                    last_seen_unix: ts,
                                },
                            );
                        s.game_type_names
                            .entry(type_name.clone())
                            .or_default()
                            .insert(
                                task.id,
                                ItemNameEntry {
                                    id: task.id,
                                    name: resolved_name.clone(),
                                    last_seen_unix: ts,
                                },
                            );
                        job.added = job.added.saturating_add(1);
                        job.pending_flush = job.pending_flush.saturating_add(1);
                    }
                }
            }

            job.cursor = job.cursor.saturating_add(1);
            processed = processed.saturating_add(1);

            if job.pending_flush >= GAME_DATA_SAVE_EVERY {
                if job.touched_item_cache {
                    save_cache(CACHE_FILE, &s.entries);
                    save_hashes_cache(&s.hashes);
                    job.touched_item_cache = false;
                }
                save_game_type_data_cache(
                    &s.game_type_data,
                    &s.game_type_hashes,
                    &s.game_type_names,
                );
                job.pending_flush = 0;
            }
        }
        job.next_step_at = Instant::now()
            + Duration::from_millis(
                if job
                    .tasks
                    .get(job.cursor)
                    .map(|t| t.link_type == encoder::LinkType::Map)
                    .unwrap_or(false)
                {
                    MAP_GAME_DATA_COOLDOWN_MS
                } else {
                    GAME_DATA_COOLDOWN_MS
                },
            );

        if job.cursor >= job.tasks.len() {
            if job.touched_item_cache {
                save_cache(CACHE_FILE, &s.entries);
                save_hashes_cache(&s.hashes);
            }
            save_game_type_data_cache(&s.game_type_data, &s.game_type_hashes, &s.game_type_names);
            s.game_type_job = None;
            s.status = if s.entries.is_empty() {
                DbStatus::NotLoaded
            } else {
                DbStatus::Loaded
            };
            s.progress = None;
            s.error_msg.clear();
        } else {
            s.status = DbStatus::Updating;
            s.progress = Some((job.cursor, job.tasks.len()));
            s.game_type_job = Some(job);
        }
    }
}

pub fn search(query: &str, only_api: bool, max_results: usize) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    let s = STATE.lock();

    s.entries
        .iter()
        .filter(|e| !only_api || e.in_api)
        .filter(|e| {
            if q.is_empty() {
                true
            } else {
                e.name.to_lowercase().contains(&q) || e.id.to_string().contains(&q)
            }
        })
        .take(max_results)
        .map(|e| SearchResult {
            id: e.id,
            name: e.name.clone(),
            in_api: e.in_api,
        })
        .collect()
}

pub fn get_item(id: u32) -> Option<InGameItem> {
    let s = STATE.lock();
    s.entries.iter().find(|e| e.id == id).cloned()
}

fn start_job(state: &mut State, mode: JobMode, start_id: u32, seed_last_found_id: u32) {
    ensure_scan_pointers(state);

    let start_id = start_id.max(1);
    let last_game_id =
        seed_last_found_id.max(state.entries.iter().map(|e| e.id).max().unwrap_or(0));
    let high_item_hint = state
        .api_ids
        .iter()
        .copied()
        .max()
        .unwrap_or_else(|| encoder::LinkType::Item.default_start());
    let frontier = last_game_id.max(high_item_hint);
    let trailing_gap = item_scan_trailing_gap(&state.entries);
    let end_id = frontier
        .saturating_add(trailing_gap)
        .max(start_id.saturating_add(trailing_gap));
    let total = end_id.saturating_sub(start_id).saturating_add(1);

    state.scan = Some(ScanJob {
        mode,
        start_id,
        current_id: start_id,
        end_id,
        last_found_id: frontier,
        trailing_gap,
        processed: 0,
        added: 0,
    });

    db::log_debug(&format!(
        "{} started {:?}: probing item ids {}..{} ({} ids, trailing gap {})",
        LOG_PREFIX, mode, start_id, end_id, total, trailing_gap
    ));
}

fn finalize_job(state: &mut State, job: ScanJob) {
    state.entries.sort_by_key(|e| e.id);
    rebuild_entry_index_in_state(state);

    save_cache(CACHE_FILE, &state.entries);
    save_hashes_cache(&state.hashes);

    state.scan = None;
    state.error_msg.clear();

    state.status = DbStatus::Loaded;
    state.progress = None;
    db::log_debug(&format!(
        "{} finished {:?}: scanned {} ids, added/updated {}",
        LOG_PREFIX, job.mode, job.processed, job.added
    ));
}

fn item_scan_trailing_gap(entries: &[InGameItem]) -> u32 {
    let mut ids: Vec<u32> = entries.iter().map(|entry| entry.id).collect();
    ids.sort_unstable();
    ids.dedup();

    let largest_observed_gap = ids
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or(0);

    largest_observed_gap
        .saturating_mul(4)
        .max(ITEM_SCAN_BOOTSTRAP_TAIL)
}

fn rebuild_entry_index(entries: &[InGameItem], out: &mut HashMap<u32, usize>) {
    out.clear();
    out.reserve(entries.len());
    for (idx, item) in entries.iter().enumerate() {
        out.insert(item.id, idx);
    }
}

fn rebuild_entry_index_in_state(state: &mut State) {
    let (entries, out) = (&state.entries, &mut state.entry_index);
    rebuild_entry_index(entries, out);
}

fn apply_saved_names(state: &mut State) {
    for entry in &mut state.entries {
        if let Some(name_entry) = state.names.get(&entry.id) {
            let saved = name_entry.name.trim();
            if !saved.is_empty()
                && !is_unresolved_decoded_text(saved)
                && !is_placeholder_name(saved)
            {
                entry.name = name_entry.name.clone();
            }
        }
    }
}

fn sanitize_loaded_names(state: &mut State) {
    state
        .names
        .retain(|_, n| !is_unresolved_decoded_text(&n.name));
    for bucket in state.game_type_names.values_mut() {
        bucket.retain(|_, n| !is_unresolved_decoded_text(&n.name));
    }
    for (ty, bucket) in state.game_type_data.iter_mut() {
        for row in bucket.values_mut() {
            if is_unresolved_decoded_text(&row.name) {
                row.name = format!("{} #{}", ty, row.id);
            }
        }
    }
}

fn is_placeholder_name(name: &str) -> bool {
    name.trim_start().starts_with("Item #")
}

fn is_generic_placeholder_name(name: &str, link_type: encoder::LinkType) -> bool {
    let t = name.trim_start();
    t.starts_with("Item #") || t.starts_with(&format!("{} #", link_type.name()))
}

fn save_hashes_cache(hashes: &HashMap<u32, ItemHashEntry>) {
    let mut values: Vec<ItemHashEntry> = hashes.values().cloned().collect();
    values.sort_by_key(|h| h.id);
    save_cache(HASHES_CACHE_FILE, &values);
}

fn save_names_cache(names: &HashMap<u32, ItemNameEntry>) {
    let mut values: Vec<ItemNameEntry> = names
        .values()
        .filter(|n| !is_unresolved_decoded_text(&n.name))
        .cloned()
        .collect();
    values.sort_by_key(|n| n.id);
    save_cache(NAMES_CACHE_FILE, &values);
}

fn save_failed_names_cache(failed: &HashMap<u32, ItemNameFailedEntry>) {
    let mut values: Vec<ItemNameFailedEntry> = failed.values().cloned().collect();
    values.sort_by_key(|n| n.id);
    save_cache(NAME_FAILED_CACHE_FILE, &values);
}

pub fn parse_game_data_for_link_type(link_type: encoder::LinkType) {
    ensure_loaded();

    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);

    let Some(content_type) = link_type_to_content_type(link_type) else {
        return;
    };

    let type_name = link_type.name().to_string();
    let ids = load_api_id_name_pairs_for_link_type(link_type);
    if ids.is_empty() {
        return;
    }

    let prop_ctx = unsafe {
        read_prop_ctx(
            s.pointers.prop_ctx_getter,
            resolve_main_thread_id(s.pointers.main_thread_match),
        )
    };
    let Some(prop_ctx) = prop_ctx else {
        return;
    };

    let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) };
    let Some(content_ctx) = content_ctx else {
        return;
    };

    let (get_content_by_index, count_defs, iterate_content_defs) = unsafe {
        match resolve_content_vfuncs(content_ctx) {
            Some(v) => v,
            None => return,
        }
    };

    let ts = now_unix();
    for (id, api_name) in ids {
        let ptr = unsafe {
            get_content_ptr_for_type_id(
                &mut s,
                link_type,
                content_ctx,
                content_type,
                id,
                Some(&api_name),
                get_content_by_index,
                count_defs,
                iterate_content_defs,
                prop_ctx,
                false,
            )
        };
        if ptr.is_null() {
            continue;
        }
        let name_hash = unsafe {
            read_non_item_name_hash(
                &mut s,
                link_type,
                content_type,
                content_ctx,
                get_content_by_index,
                count_defs,
                iterate_content_defs,
                ptr,
                &api_name,
                prop_ctx,
            )
        };
        let mut decoded_name = s
            .game_type_names
            .get(&type_name)
            .and_then(|m| m.get(&id))
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if decoded_name.trim().is_empty() || is_unresolved_decoded_text(&decoded_name) {
            decoded_name = format!("{} #{}", link_type.name(), id);
        }
        let (skin_rarity_code, skin_flags_code, skin_type_code) =
            if link_type == encoder::LinkType::Wardrobe {
                unsafe { read_skin_metadata(ptr, id) }.unwrap_or((0, 0, 0))
            } else {
                (0, 0, 0)
            };
        let (poi_type_code, map_name_hash, map_name) = if link_type == encoder::LinkType::Map {
            unsafe { read_poi_metadata(&mut s, ptr, id, prop_ctx) }.unwrap_or((0, 0, String::new()))
        } else {
            (0, 0, String::new())
        };
        let description_hash = 0;
        let decoded_desc = String::new();
        let bucket = s.game_type_data.entry(type_name.clone()).or_default();
        bucket.insert(
            id,
            GameTypeDataEntry {
                link_type: type_name.clone(),
                id,
                name: decoded_name.clone(),
                in_api: true,
                name_hash,
                description_hash,
                description: decoded_desc,
                skin_rarity_code,
                skin_flags_code,
                skin_type_code,
                poi_type_code,
                map_name_hash,
                map_name,
                last_seen_unix: ts,
            },
        );
        s.game_type_hashes
            .entry(type_name.clone())
            .or_default()
            .insert(
                id,
                ItemHashEntry {
                    id,
                    content_index: id,
                    name_hash,
                    description_hash,
                    upgrade_name_hash: 0,
                    base_upgrade_item_id: 0,
                    last_seen_unix: ts,
                },
            );
        s.game_type_names
            .entry(type_name.clone())
            .or_default()
            .insert(
                id,
                ItemNameEntry {
                    id,
                    name: decoded_name.clone(),
                    last_seen_unix: ts,
                },
            );
    }
    save_game_type_data_cache(&s.game_type_data, &s.game_type_hashes, &s.game_type_names);
}

pub fn get_game_data_for_link_type(
    link_type: encoder::LinkType,
    query: &str,
    max_results: usize,
) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    let s = STATE.lock();
    let type_name = link_type.name();

    if link_type == encoder::LinkType::Item {
        return s
            .entries
            .iter()
            .filter(|e| {
                if q.is_empty() {
                    true
                } else {
                    e.name.to_lowercase().contains(&q) || e.id.to_string().contains(&q)
                }
            })
            .take(max_results)
            .map(|e| SearchResult {
                id: e.id,
                name: e.name.clone(),
                in_api: e.in_api,
            })
            .collect();
    }

    s.game_type_data
        .get(type_name)
        .map(|m| {
            let mut rows: Vec<SearchResult> = m
                .values()
                .filter(|e| {
                    if q.is_empty() {
                        true
                    } else {
                        e.name.to_lowercase().contains(&q) || e.id.to_string().contains(&q)
                    }
                })
                .map(|e| SearchResult {
                    id: e.id,
                    name: e.name.clone(),
                    in_api: e.in_api,
                })
                .collect();
            rows.sort_by_key(|r| r.id);
            rows.into_iter().take(max_results).collect()
        })
        .unwrap_or_default()
}

pub fn get_game_data_entry_for_link_type(
    link_type: encoder::LinkType,
    id: u32,
) -> Option<GameTypeDataEntry> {
    let s = STATE.lock();
    s.game_type_data
        .get(link_type.name())
        .and_then(|m| m.get(&id).cloned())
}

pub fn debug_probe_content(
    link_type: encoder::LinkType,
    id: u32,
) -> Result<ContentProbeDebugInfo, String> {
    debug_probe_content_for_content_type(link_type, id, None)
}

pub fn debug_probe_content_for_content_type(
    link_type: encoder::LinkType,
    id: u32,
    content_type_override: Option<u32>,
) -> Result<ContentProbeDebugInfo, String> {
    ensure_loaded();

    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);

    let Some(default_type) = link_type_to_content_type(link_type) else {
        return Err("Unsupported link type".to_string());
    };
    let content_type = content_type_override.unwrap_or(default_type);

    let prop_ctx = unsafe {
        read_prop_ctx(
            s.pointers.prop_ctx_getter,
            resolve_main_thread_id(s.pointers.main_thread_match),
        )
    }
    .ok_or_else(|| "PropContext unavailable (must be in-game on a character)".to_string())?;

    let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) }
        .ok_or_else(|| "Could not resolve ContentCtx".to_string())?;

    let (get_content_by_index, count_defs, iterate_content_defs) = unsafe {
        resolve_content_vfuncs(content_ctx)
            .ok_or_else(|| "Could not resolve ContentCtx vfuncs".to_string())?
    };

    let api_name = get_api_name_for_link_type_id(link_type, id);
    let resolved_content_type = content_type;
    let ptr = unsafe {
        get_strict_content_ptr_for_debug(
            &mut s,
            link_type,
            content_ctx,
            content_type,
            id,
            api_name.as_deref(),
            get_content_by_index,
            count_defs,
            iterate_content_defs,
            prop_ctx,
        )
    };
    if ptr.is_null() {
        return Err(format!(
            "No content ptr for type {} id {}",
            link_type.name(),
            id
        ));
    }

    if unsafe { !is_readable(ptr, 0x90) } {
        return Err("Content ptr is not readable".to_string());
    }

    let content_def_type = unsafe { (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned() };
    let content_def_index = unsafe { (ptr.add(CONTENT_DEF_INDEX) as *const u32).read_unaligned() };

    let known_name_offset = known_name_hash_offset_for_link_type(link_type);
    let observed_max_hash = s
        .hashes
        .values()
        .flat_map(|e| [e.name_hash, e.description_hash])
        .chain(
            s.game_type_hashes
                .values()
                .flat_map(|m| m.values().flat_map(|e| [e.name_hash, e.description_hash])),
        )
        .filter(|h| *h > 0)
        .max()
        .unwrap_or(1_200_000);
    let soft_cap = ((observed_max_hash as u64 * 3) / 2 + 8192) as u32;
    let hard_cap = soft_cap.saturating_mul(4);

    let mut rows = Vec::new();
    let is_known_hash_for_selected_type = |h: u32| -> bool {
        if h == 0 {
            return false;
        }
        if link_type == encoder::LinkType::Item {
            return s
                .hashes
                .values()
                .any(|e| e.name_hash == h || e.description_hash == h || e.upgrade_name_hash == h);
        }
        s.game_type_hashes
            .get(link_type.name())
            .map(|m| {
                m.values()
                    .any(|e| e.name_hash == h || e.description_hash == h)
            })
            .unwrap_or(false)
    };
    for offset in (0x20usize..=0x88usize).step_by(4) {
        let raw_u32 = unsafe { (ptr.add(offset) as *const u32).read_unaligned() };
        // Fallback pattern for types with no stored hashes:
        // - exclude zero/self-id/pointer-like values
        // - stay within learned hash range from gathered data
        // - be slightly more permissive at known name-hash offsets
        let looks_like_hash_pattern = if raw_u32 == id {
            false
        } else if !is_plausible_text_hash(raw_u32) {
            false
        } else if Some(offset) == known_name_offset {
            raw_u32 >= 4 && raw_u32 <= hard_cap
        } else {
            raw_u32 >= 4 && raw_u32 <= soft_cap
        };
        let is_hash_candidate = is_known_hash_for_selected_type(raw_u32) || looks_like_hash_pattern;
        let candidate_preview = String::new();
        rows.push(ContentOffsetProbeRow {
            offset,
            raw_u32,
            is_hash_candidate,
            candidate_preview,
        });
    }

    Ok(ContentProbeDebugInfo {
        link_type,
        id,
        content_type,
        resolved_content_type,
        content_ptr: ptr as usize as u64,
        content_def_type,
        content_def_index,
        known_name_offset: known_name_hash_offset_for_link_type(link_type),
        rows,
        subdef_ptr: 0,
        subdef_rows: Vec::new(),
    })
}

pub fn debug_probe_item_subdef_for_content_type(
    link_type: encoder::LinkType,
    id: u32,
    content_type_override: Option<u32>,
) -> Result<ContentProbeDebugInfo, String> {
    if link_type != encoder::LinkType::Item {
        return Err("Subdef probe is only available for Item type".to_string());
    }

    let mut info = debug_probe_content_for_content_type(link_type, id, content_type_override)?;

    let mut s = STATE.lock();
    ensure_scan_pointers(&mut s);

    let prop_ctx = unsafe {
        read_prop_ctx(
            s.pointers.prop_ctx_getter,
            resolve_main_thread_id(s.pointers.main_thread_match),
        )
    }
    .ok_or_else(|| "PropContext unavailable (must be in-game on a character)".to_string())?;

    let content_ctx = unsafe { read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) }
        .ok_or_else(|| "Could not resolve ContentCtx".to_string())?;

    let (get_content_by_index, count_defs, iterate_content_defs) = unsafe {
        resolve_content_vfuncs(content_ctx)
            .ok_or_else(|| "Could not resolve ContentCtx vfuncs".to_string())?
    };

    let Some(default_type) = link_type_to_content_type(link_type) else {
        return Err("Unsupported link type".to_string());
    };
    let content_type = content_type_override.unwrap_or(default_type);
    let api_name = get_api_name_for_link_type_id(link_type, id);
    let ptr = unsafe {
        get_strict_content_ptr_for_debug(
            &mut s,
            link_type,
            content_ctx,
            content_type,
            id,
            api_name.as_deref(),
            get_content_by_index,
            count_defs,
            iterate_content_defs,
            prop_ctx,
        )
    };
    if ptr.is_null() {
        return Err(format!(
            "No content ptr for type {} id {}",
            link_type.name(),
            id
        ));
    }

    let subdef_ptr = unsafe { read_ptr(ptr, ITEM_DEF_SUBDEF) }.unwrap_or(std::ptr::null());
    if subdef_ptr.is_null() {
        return Err("Item subdef pointer is null".to_string());
    }
    if unsafe { !is_readable(subdef_ptr, 0xA4) } {
        return Err("Item subdef pointer is not readable".to_string());
    }

    let mut rows = Vec::new();
    for offset in (0x0usize..=0xA0usize).step_by(4) {
        let raw_u32 = unsafe { (subdef_ptr.add(offset) as *const u32).read_unaligned() };
        // Subdef debug should stay strict: only expose known text-hash slots
        // used for upgrade-like subdefs. This avoids noisy false positives.
        let looks_like_hash = (offset == ITEM_UPGRADE_TEXT_HASH1
            || offset == ITEM_UPGRADE_TEXT_HASH2)
            && raw_u32 != id
            && is_plausible_text_hash(raw_u32);
        rows.push(ContentOffsetProbeRow {
            offset,
            raw_u32,
            is_hash_candidate: looks_like_hash,
            candidate_preview: String::new(),
        });
    }

    info.subdef_ptr = subdef_ptr as usize as u64;
    info.subdef_rows = rows;
    Ok(info)
}

fn link_type_to_content_type(link_type: encoder::LinkType) -> Option<u32> {
    match link_type {
        encoder::LinkType::Item => Some(34),     // ItemDef
        encoder::LinkType::Map => Some(51),      // PointOfInterestDef (waypoints/POI)
        encoder::LinkType::Skill => Some(63),    // SkillDef
        encoder::LinkType::Trait => Some(79),    // TraitDef
        encoder::LinkType::Recipe => Some(13),   // CraftingRecipeDef
        encoder::LinkType::Wardrobe => Some(65), // SkinDef
        encoder::LinkType::Outfit => Some(50),   // OutfitDef
    }
}

fn load_api_id_name_pairs_for_link_type(link_type: encoder::LinkType) -> Vec<(u32, String)> {
    match link_type {
        encoder::LinkType::Item => load_cache::<Vec<db::items::Item>>("db_api_items.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
        encoder::LinkType::Map => load_cache::<Vec<db::pois::Poi>>("db_api_pois.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
        encoder::LinkType::Skill => load_cache::<Vec<db::skills::Skill>>("db_api_skills.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
        encoder::LinkType::Trait => load_cache::<Vec<db::traits::GwTrait>>("db_api_traits.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
        encoder::LinkType::Recipe => load_cache::<Vec<db::recipes::Recipe>>("db_api_recipes.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.output_item_name))
            .collect(),
        encoder::LinkType::Wardrobe => load_cache::<Vec<db::skins::Skin>>("db_api_skins.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
        encoder::LinkType::Outfit => load_cache::<Vec<db::outfits::Outfit>>("db_api_outfits.json")
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name))
            .collect(),
    }
}

fn get_api_name_for_link_type_id(link_type: encoder::LinkType, id: u32) -> Option<String> {
    load_api_id_name_pairs_for_link_type(link_type)
        .into_iter()
        .find_map(|(x, name)| if x == id { Some(name) } else { None })
}

fn save_game_type_data_cache(
    data: &HashMap<String, HashMap<u32, GameTypeDataEntry>>,
    hashes: &HashMap<String, HashMap<u32, ItemHashEntry>>,
    names: &HashMap<String, HashMap<u32, ItemNameEntry>>,
) {
    for &lt in encoder::LinkType::ALL {
        if lt == encoder::LinkType::Item {
            continue;
        }
        let key = lt.name();
        let file = game_type_data_file_for_link_type(lt);
        if let Some(rows) = data.get(key) {
            let mut out: Vec<GameTypeDataEntry> = rows
                .values()
                .cloned()
                .map(|mut r| {
                    if is_unresolved_decoded_text(&r.name) {
                        r.name = format!("{} #{}", lt.name(), r.id);
                    }
                    r
                })
                .collect();
            out.sort_by_key(|r| r.id);
            save_cache(file, &out);
        } else {
            delete_cache_local(file);
        }

        let hash_file = game_type_hashes_file_for_link_type(lt);
        if let Some(rows) = hashes.get(key) {
            let mut out: Vec<ItemHashEntry> = rows.values().cloned().collect();
            out.sort_by_key(|r| r.id);
            save_cache(hash_file, &out);
        } else {
            delete_cache_local(hash_file);
        }

        let names_file = game_type_names_file_for_link_type(lt);
        if let Some(rows) = names.get(key) {
            let mut out: Vec<ItemNameEntry> = rows
                .values()
                .filter(|r| !is_unresolved_decoded_text(&r.name))
                .cloned()
                .collect();
            out.sort_by_key(|r| r.id);
            save_cache(names_file, &out);
        } else {
            delete_cache_local(names_file);
        }
    }
    // Ensure deprecated aggregate file is not used anymore.
    delete_cache_local(LEGACY_GAME_TYPE_DATA_FILE);
}

fn game_type_data_file_for_link_type(link_type: encoder::LinkType) -> &'static str {
    match link_type {
        encoder::LinkType::Item => "db_ingame_items_data.json",
        encoder::LinkType::Map => "db_ingame_map_data.json",
        encoder::LinkType::Skill => "db_ingame_skill_data.json",
        encoder::LinkType::Trait => "db_ingame_trait_data.json",
        encoder::LinkType::Recipe => "db_ingame_recipe_data.json",
        encoder::LinkType::Wardrobe => "db_ingame_wardrobe_data.json",
        encoder::LinkType::Outfit => "db_ingame_outfit_data.json",
    }
}

fn game_type_hashes_file_for_link_type(link_type: encoder::LinkType) -> &'static str {
    match link_type {
        encoder::LinkType::Item => "db_ingame_item_hashes.json",
        encoder::LinkType::Map => "db_ingame_poi_hashes.json",
        encoder::LinkType::Skill => "db_ingame_skill_hashes.json",
        encoder::LinkType::Trait => "db_ingame_trait_hashes.json",
        encoder::LinkType::Recipe => "db_ingame_recipe_hashes.json",
        encoder::LinkType::Wardrobe => "db_ingame_wardrobe_hashes.json",
        encoder::LinkType::Outfit => "db_ingame_outfit_hashes.json",
    }
}

fn game_type_names_file_for_link_type(link_type: encoder::LinkType) -> &'static str {
    match link_type {
        encoder::LinkType::Item => "db_ingame_item_names.json",
        encoder::LinkType::Map => "db_ingame_poi_names.json",
        encoder::LinkType::Skill => "db_ingame_skill_names.json",
        encoder::LinkType::Trait => "db_ingame_trait_names.json",
        encoder::LinkType::Recipe => "db_ingame_recipe_names.json",
        encoder::LinkType::Wardrobe => "db_ingame_wardrobe_names.json",
        encoder::LinkType::Outfit => "db_ingame_outfit_names.json",
    }
}

unsafe fn read_item_entry(
    state: &State,
    item_ptr: *const u8,
    fallback_id: u32,
) -> Option<(InGameItem, u32, u32, u32, u32, u32)> {
    if !is_readable(item_ptr, ITEM_DEF_IS_GEMSTORE + 1) {
        return None;
    }

    let id = (item_ptr.add(ITEM_DEF_ID) as *const u32).read_unaligned();
    let id = if id == 0 { fallback_id } else { id };
    if id == 0 {
        return None;
    }

    let content_type = (item_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if content_type != ITEM_CONTENT_TYPE {
        return None;
    }

    let content_index = (item_ptr.add(CONTENT_DEF_INDEX) as *const u32).read_unaligned();

    let item_type_code = (item_ptr.add(ITEM_DEF_TYPE) as *const u32).read_unaligned();
    let rarity_code = (item_ptr.add(ITEM_DEF_RARITY) as *const u32).read_unaligned();
    let required_level = (item_ptr.add(ITEM_DEF_REQUIRED_LEVEL) as *const u32).read_unaligned();
    let name_hash = (item_ptr.add(ITEM_DEF_NAME_HASH) as *const u32).read_unaligned();
    let description_hash = (item_ptr.add(ITEM_DEF_DESCRIPTION_HASH) as *const u32).read_unaligned();
    let vendor_value = (item_ptr.add(ITEM_DEF_VENDOR_VALUE) as *const u32).read_unaligned();
    let is_gemstore = *item_ptr.add(ITEM_DEF_IS_GEMSTORE) != 0;
    let default_skin_id = read_item_default_skin_id(item_ptr);
    let upgrade_name_hash = 0;
    let base_upgrade_item_id = if item_type_code == 23 {
        let candidate = read_item_base_upgrade_item_id(item_ptr);
        if candidate > 0 {
            candidate
        } else {
            0
        }
    } else {
        0
    };

    let name = format!("Item #{}", id);
    let description = String::new();

    Some((
        InGameItem {
            id,
            name,
            description,
            item_type_code,
            rarity_code,
            required_level,
            vendor_value,
            is_gemstore,
            default_skin_id,
            upgrade_name: String::new(),
            chat_link: encoder::generate_batch_link(encoder::LinkType::Item, id),
            in_api: state.api_ids.contains(&id),
        },
        content_index,
        name_hash,
        description_hash,
        upgrade_name_hash,
        base_upgrade_item_id,
    ))
}

unsafe fn read_item_upgrade_name_hash(item_ptr: *const u8) -> u32 {
    let Some(subdef_ptr) = read_ptr(item_ptr, ITEM_DEF_SUBDEF) else {
        return 0;
    };
    if subdef_ptr.is_null() {
        return 0;
    }

    if is_readable(
        subdef_ptr,
        ITEM_UPGRADE_TEXT_HASH1 + std::mem::size_of::<u32>(),
    ) {
        let h1 = (subdef_ptr.add(ITEM_UPGRADE_TEXT_HASH1) as *const u32).read_unaligned();
        if h1 != 0 {
            return h1;
        }
    }
    if is_readable(
        subdef_ptr,
        ITEM_UPGRADE_TEXT_HASH2 + std::mem::size_of::<u32>(),
    ) {
        let h2 = (subdef_ptr.add(ITEM_UPGRADE_TEXT_HASH2) as *const u32).read_unaligned();
        if h2 != 0 {
            return h2;
        }
    }
    0
}

unsafe fn read_item_base_upgrade_item_id(item_ptr: *const u8) -> u32 {
    let Some(subdef_ptr) = read_ptr(item_ptr, ITEM_DEF_SUBDEF) else {
        return 0;
    };
    if subdef_ptr.is_null()
        || !is_readable(
            subdef_ptr,
            ITEM_UPGRADE_BASE_ITEM_ID + std::mem::size_of::<u32>(),
        )
    {
        return 0;
    }
    (subdef_ptr.add(ITEM_UPGRADE_BASE_ITEM_ID) as *const u32).read_unaligned()
}

unsafe fn refresh_item_fallback_ids(
    state: &mut State,
    id: u32,
    content_ctx: *const u8,
    get_content_by_index: GetContentByIndexFn,
) {
    let ptr = get_content_by_index(content_ctx, ITEM_CONTENT_TYPE, id);
    if ptr.is_null() || !is_readable(ptr, ITEM_DEF_SUBDEF + std::mem::size_of::<u64>()) {
        return;
    }
    let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if c_type != ITEM_CONTENT_TYPE {
        return;
    }

    let default_skin_id = read_item_default_skin_id(ptr);
    let item_type_code = (ptr.add(ITEM_DEF_TYPE) as *const u32).read_unaligned();
    let base_upgrade_item_id = if item_type_code == 23 {
        let candidate = read_item_base_upgrade_item_id(ptr);
        if candidate > 0 {
            candidate
        } else {
            0
        }
    } else {
        0
    };

    if let Some(&idx) = state.entry_index.get(&id) {
        if let Some(entry) = state.entries.get_mut(idx) {
            if default_skin_id != 0 {
                entry.default_skin_id = default_skin_id;
            }
        }
    }

    if default_skin_id == 0 && base_upgrade_item_id == 0 {
        return;
    }

    let ts = now_unix();
    state
        .hashes
        .entry(id)
        .and_modify(|h| {
            if base_upgrade_item_id != 0 {
                h.base_upgrade_item_id = base_upgrade_item_id;
            }
            h.last_seen_unix = ts;
        })
        .or_insert(ItemHashEntry {
            id,
            content_index: id,
            name_hash: 0,
            description_hash: 0,
            upgrade_name_hash: 0,
            base_upgrade_item_id,
            last_seen_unix: ts,
        });
}

unsafe fn resolve_item_name_fallback_from_skin(
    state: &mut State,
    _item_id: u32,
    default_skin_id: u32,
) -> Option<String> {
    if default_skin_id == 0 {
        return None;
    }

    let wardrobe_key = encoder::LinkType::Wardrobe.name().to_string();
    if let Some(name) = state
        .game_type_names
        .get(&wardrobe_key)
        .and_then(|m| m.get(&default_skin_id))
        .map(|e| e.name.trim().to_string())
        .filter(|n| {
            !n.is_empty()
                && !is_generic_placeholder_name(n, encoder::LinkType::Wardrobe)
                && !is_unresolved_decoded_text(n)
        })
    {
        return Some(name);
    }
    if let Some(name) = state
        .game_type_data
        .get(&wardrobe_key)
        .and_then(|m| m.get(&default_skin_id))
        .map(|e| e.name.trim().to_string())
        .filter(|n| {
            !n.is_empty()
                && !is_generic_placeholder_name(n, encoder::LinkType::Wardrobe)
                && !is_unresolved_decoded_text(n)
        })
    {
        return Some(name);
    }
    if let Some(name_hash) = state
        .game_type_hashes
        .get(&wardrobe_key)
        .and_then(|m| m.get(&default_skin_id))
        .map(|h| h.name_hash)
    {
        if let Some(name) = state
            .decoded_text_by_hash
            .get(&name_hash)
            .map(|s| s.trim().to_string())
            .filter(|n| !n.is_empty() && !is_unresolved_decoded_text(n))
        {
            return Some(name);
        }
    }
    None
}

unsafe fn resolve_upgrade_name_fallback_from_base_upgrade_item(
    state: &mut State,
    item_id: u32,
    item_content_access: Option<(*const u8, GetContentByIndexFn)>,
) -> Option<String> {
    let base_upgrade_id = state
        .hashes
        .get(&item_id)
        .map(|h| h.base_upgrade_item_id)
        .unwrap_or(0);
    if base_upgrade_id == 0 {
        return None;
    }

    if let Some(name) = state
        .names
        .get(&base_upgrade_id)
        .map(|e| e.name.trim().to_string())
        .filter(|n| !n.is_empty() && !is_placeholder_name(n) && !is_unresolved_decoded_text(n))
    {
        return Some(name);
    }
    if let Some(name) = state
        .entry_index
        .get(&base_upgrade_id)
        .and_then(|&idx| state.entries.get(idx))
        .map(|e| e.name.trim().to_string())
        .filter(|n| !n.is_empty() && !is_placeholder_name(n) && !is_unresolved_decoded_text(n))
    {
        return Some(name);
    }

    if let Some((content_ctx, get_content_by_index)) = item_content_access {
        let base_default_skin_id = state
            .entry_index
            .get(&base_upgrade_id)
            .and_then(|&idx| state.entries.get(idx))
            .map(|e| e.default_skin_id)
            .unwrap_or(0);
        let need_base_refresh = base_default_skin_id == 0
            || state
                .hashes
                .get(&base_upgrade_id)
                .map(|h| h.name_hash == 0)
                .unwrap_or(true);
        if need_base_refresh {
            refresh_item_fallback_ids(state, base_upgrade_id, content_ctx, get_content_by_index);
        }
    }

    let base_default_skin_id = state
        .entry_index
        .get(&base_upgrade_id)
        .and_then(|&idx| state.entries.get(idx))
        .map(|e| e.default_skin_id)
        .unwrap_or(0);
    if let Some(name) =
        resolve_item_name_fallback_from_skin(state, base_upgrade_id, base_default_skin_id)
    {
        return Some(name);
    }

    let base_name_hash = state
        .hashes
        .get(&base_upgrade_id)
        .map(|h| h.name_hash)
        .unwrap_or(0);
    if base_name_hash == 0 {
        return Some(format!("Item #{}", base_upgrade_id));
    }

    let decoded = if let Some(cached) = state.decoded_text_by_hash.get(&base_name_hash).cloned() {
        Some(cached)
    } else {
        None
    };

    let resolved = decoded
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !is_unresolved_decoded_text(s))
        .unwrap_or_else(|| format!("Item #{}", base_upgrade_id));

    if !is_placeholder_name(&resolved) {
        let ts = now_unix();
        state.names.insert(
            base_upgrade_id,
            ItemNameEntry {
                id: base_upgrade_id,
                name: resolved.clone(),
                last_seen_unix: ts,
            },
        );
        if let Some(&idx) = state.entry_index.get(&base_upgrade_id) {
            if let Some(entry) = state.entries.get_mut(idx) {
                entry.name = resolved.clone();
            }
        }
    }

    Some(resolved)
}

unsafe fn read_item_default_skin_id(item_ptr: *const u8) -> u32 {
    let Some(subdef_ptr) = read_ptr(item_ptr, ITEM_DEF_SUBDEF) else {
        return 0;
    };
    if subdef_ptr.is_null() {
        return 0;
    }

    // ItemDef::subdef points to ItemWeapon/ItemArmor/etc. Both weapon/armor start with SkinDef* at +0x0.
    let Some(skin_ptr) = read_ptr(subdef_ptr, 0x0) else {
        return 0;
    };
    if skin_ptr.is_null() || !is_readable(skin_ptr, ITEM_DEF_ID + std::mem::size_of::<u32>()) {
        return 0;
    }

    let expected_skin_type = link_type_to_content_type(encoder::LinkType::Wardrobe).unwrap_or(65);
    if is_readable(skin_ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
        let c_type = (skin_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != expected_skin_type {
            return 0;
        }
    }

    (skin_ptr.add(ITEM_DEF_ID) as *const u32).read_unaligned()
}

unsafe fn read_skin_metadata(skin_ptr: *const u8, expected_id: u32) -> Option<(u32, u32, u32)> {
    if !is_readable(skin_ptr, SKIN_DEF_TYPE + std::mem::size_of::<u32>()) {
        return None;
    }
    let content_type = (skin_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if content_type != link_type_to_content_type(encoder::LinkType::Wardrobe).unwrap_or_default() {
        return None;
    }
    let skin_id = (skin_ptr.add(ITEM_DEF_ID) as *const u32).read_unaligned();
    if skin_id != 0 && skin_id != expected_id {
        return None;
    }
    let rarity_code = (skin_ptr.add(SKIN_DEF_RARITY) as *const u32).read_unaligned();
    let flags_code = (skin_ptr.add(SKIN_DEF_FLAGS) as *const u32).read_unaligned();
    let skin_type_code = (skin_ptr.add(SKIN_DEF_TYPE) as *const u32).read_unaligned();
    Some((rarity_code, flags_code, skin_type_code))
}

unsafe fn read_poi_metadata(
    state: &mut State,
    poi_ptr: *const u8,
    expected_id: u32,
    _prop_ctx: *const u8,
) -> Option<(u32, u32, String)> {
    if !is_readable(poi_ptr, POI_DEF_TYPE + std::mem::size_of::<u32>()) {
        return None;
    }

    let content_type = (poi_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if content_type != link_type_to_content_type(encoder::LinkType::Map).unwrap_or_default() {
        return None;
    }

    let poi_id = (poi_ptr.add(POI_DEF_ID) as *const u32).read_unaligned();
    if poi_id != 0 && poi_id != expected_id {
        return None;
    }

    let poi_type_code = (poi_ptr.add(POI_DEF_TYPE) as *const u32).read_unaligned();
    let Some(map_ptr) = read_ptr(poi_ptr, POI_DEF_MAP_PTR) else {
        return Some((poi_type_code, 0, String::new()));
    };
    let map_ptr_key = map_ptr as usize;
    if let Some(&cached_hash) = state.mapdef_name_hash_by_ptr.get(&map_ptr_key) {
        return Some((poi_type_code, cached_hash, String::new()));
    }
    if map_ptr.is_null() || !is_readable(map_ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
        state.mapdef_name_hash_by_ptr.insert(map_ptr_key, 0);
        return Some((poi_type_code, 0, String::new()));
    }

    let map_def_type = (map_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if map_def_type != MAP_DEF_CONTENT_TYPE {
        state.mapdef_name_hash_by_ptr.insert(map_ptr_key, 0);
        return Some((poi_type_code, 0, String::new()));
    }

    // Exact MapDef.Name (TextHash) offset from GW2RE.
    if !is_readable(map_ptr, MAP_DEF_NAME_HASH + std::mem::size_of::<u32>()) {
        state.mapdef_name_hash_by_ptr.insert(map_ptr_key, 0);
        return Some((poi_type_code, 0, String::new()));
    }
    let hash = (map_ptr.add(MAP_DEF_NAME_HASH) as *const u32).read_unaligned();
    state.mapdef_name_hash_by_ptr.insert(map_ptr_key, hash);
    if hash != 0 {
        return Some((poi_type_code, hash, String::new()));
    }

    state.mapdef_name_hash_by_ptr.insert(map_ptr_key, 0);
    Some((poi_type_code, 0, String::new()))
}

unsafe fn read_non_item_name_hash(
    state: &mut State,
    link_type: encoder::LinkType,
    content_type: u32,
    content_ctx: *const u8,
    get_content_by_index: GetContentByIndexFn,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
    ptr: *const u8,
    api_name: &str,
    prop_ctx: *const u8,
) -> u32 {
    if link_type == encoder::LinkType::Map {
        if is_readable(ptr, POI_DEF_NAME_HASH + std::mem::size_of::<u32>()) {
            return (ptr.add(POI_DEF_NAME_HASH) as *const u32).read_unaligned();
        }
        return 0;
    }
    if link_type == encoder::LinkType::Wardrobe {
        if is_readable(ptr, 0x68 + std::mem::size_of::<u32>()) {
            return (ptr.add(0x68) as *const u32).read_unaligned();
        }
        return 0;
    }

    if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
        return 0;
    }
    let ty = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if ty != content_type {
        return 0;
    }

    let type_name = link_type.name().to_string();
    let offset = known_name_hash_offset_for_link_type(link_type)
        .or_else(|| state.discovered_name_hash_offsets.get(&type_name).copied())
        .or_else(|| {
            let discovered = discover_name_hash_offset_for_link_type(
                state,
                link_type,
                content_type,
                content_ctx,
                get_content_by_index,
                count_defs,
                iterate_content_defs,
                prop_ctx,
            );
            if let Some(off) = discovered {
                state
                    .discovered_name_hash_offsets
                    .insert(type_name.clone(), off);
            }
            discovered
        });
    let Some(off) = offset else {
        let _ = api_name;
        return 0;
    };
    if !is_readable(ptr, off + std::mem::size_of::<u32>()) {
        return 0;
    }
    (ptr.add(off) as *const u32).read_unaligned()
}

unsafe fn decode_poi_name_from_def(
    state: &State,
    poi_ptr: *const u8,
    expected_id: u32,
    prop_ctx: *const u8,
) -> Option<(u32, String)> {
    if !is_readable(poi_ptr, POI_DEF_NAME_HASH + std::mem::size_of::<u32>()) {
        return None;
    }

    let content_type = (poi_ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if content_type != link_type_to_content_type(encoder::LinkType::Map).unwrap_or_default() {
        return None;
    }

    let poi_id = (poi_ptr.add(POI_DEF_ID) as *const u32).read_unaligned();
    if poi_id != 0 && poi_id != expected_id {
        return None;
    }

    let text_hash = (poi_ptr.add(POI_DEF_NAME_HASH) as *const u32).read_unaligned();
    if text_hash == 0 {
        return None;
    }

    let decoded = decode_text_hash(state, text_hash, prop_ctx)?;
    let name = decoded.trim();
    if name.is_empty() {
        None
    } else {
        Some((text_hash, name.to_string()))
    }
}

fn known_name_hash_offset_for_link_type(link_type: encoder::LinkType) -> Option<usize> {
    match link_type {
        encoder::LinkType::Map => Some(POI_DEF_NAME_HASH), // PointOfInterestDef.Name
        encoder::LinkType::Skill => Some(0x34),            // SkillDef.Name
        encoder::LinkType::Trait => Some(0x64),            // TraitDef.Name
        encoder::LinkType::Wardrobe => Some(0x68),         // SkinDef.Name
        _ => None,
    }
}

fn normalize_api_name_for_match(name: &str) -> String {
    let mut out = name.trim();
    if let Some(rest) = out.strip_prefix('(') {
        if let Some(end) = rest.find(')') {
            let maybe_num = &rest[..end];
            if maybe_num.chars().all(|c| c.is_ascii_digit()) {
                out = rest[end + 1..].trim_start();
            }
        }
    }
    out.to_lowercase()
}

fn normalize_decoded_name_for_match(name: &str) -> String {
    name.trim().to_lowercase()
}

unsafe fn decode_text_hash_at_offset(
    state: &State,
    ptr: *const u8,
    offset: usize,
    prop_ctx: *const u8,
) -> Option<(u32, String)> {
    if !is_readable(ptr, offset + std::mem::size_of::<u32>()) {
        return None;
    }
    let text_hash = (ptr.add(offset) as *const u32).read_unaligned();
    if !is_plausible_text_hash(text_hash) {
        return None;
    }
    let decoded = decode_text_hash(state, text_hash, prop_ctx)?;
    let decoded = decoded.trim();
    if decoded.is_empty() || is_unresolved_decoded_text(decoded) {
        return None;
    }
    Some((text_hash, decoded.to_string()))
}

unsafe fn resolve_text_hash_coded_only(
    state: &State,
    text_hash: u32,
    prop_ctx: *const u8,
) -> Option<String> {
    if !is_plausible_text_hash(text_hash) {
        return None;
    }
    let coded_ptr = resolve_coded_text_ptr(state, text_hash, prop_ctx);
    if coded_ptr.is_null() {
        return None;
    }
    let text = read_wide_string(coded_ptr, 1024)?;
    let trimmed = text.trim();
    if trimmed.is_empty() || is_unresolved_decoded_text(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

unsafe fn discover_name_hash_offset_for_link_type(
    state: &mut State,
    link_type: encoder::LinkType,
    content_type: u32,
    content_ctx: *const u8,
    get_content_by_index: GetContentByIndexFn,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
    prop_ctx: *const u8,
) -> Option<usize> {
    // Candidate offsets for common TextHash fields across content defs.
    const CANDIDATE_OFFSETS: &[usize] = &[
        0x2C, 0x30, 0x34, 0x38, 0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58, 0x5C, 0x60, 0x64,
        0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C, 0x80, 0x84,
    ];

    let pairs = load_api_id_name_pairs_for_link_type(link_type);
    if pairs.is_empty() {
        return None;
    }

    let mut samples: Vec<(*const u8, String)> = Vec::new();
    for (id, api_name) in pairs.into_iter().take(96) {
        let ptr = get_content_ptr_for_type_id(
            state,
            link_type,
            content_ctx,
            content_type,
            id,
            Some(&api_name),
            get_content_by_index,
            count_defs,
            iterate_content_defs,
            prop_ctx,
            false,
        );
        if ptr.is_null() {
            continue;
        }
        if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
            continue;
        }
        let ty = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if ty != content_type {
            continue;
        }
        samples.push((ptr, normalize_api_name_for_match(&api_name)));
        if samples.len() >= 14 {
            break;
        }
    }
    if samples.len() < 3 {
        return None;
    }

    let mut best: Option<(usize, usize, usize)> = None; // (offset, matches, tested)

    for &off in CANDIDATE_OFFSETS {
        let mut matches = 0usize;
        let mut tested = 0usize;
        for (ptr, api_name_norm) in samples.iter().take(10) {
            let Some((_, decoded)) = decode_text_hash_at_offset(state, *ptr, off, prop_ctx) else {
                continue;
            };
            tested += 1;
            if normalize_decoded_name_for_match(&decoded) == *api_name_norm {
                matches += 1;
            }
        }
        if tested == 0 {
            continue;
        }
        let beats_current = match best {
            Some((_, best_matches, best_tested)) => {
                matches > best_matches || (matches == best_matches && tested > best_tested)
            }
            None => true,
        };
        if beats_current {
            best = Some((off, matches, tested));
        }
    }

    let (off, matches, tested) = best?;
    if matches >= 2 && matches * 2 >= tested {
        Some(off)
    } else {
        None
    }
}

unsafe fn decode_non_item_name(
    state: &mut State,
    link_type: encoder::LinkType,
    content_type: u32,
    content_ctx: *const u8,
    get_content_by_index: GetContentByIndexFn,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
    ptr: *const u8,
    expected_id: u32,
    api_name: &str,
    prop_ctx: *const u8,
) -> (u32, String) {
    if link_type == encoder::LinkType::Map {
        return decode_poi_name_from_def(state, ptr, expected_id, prop_ctx)
            .unwrap_or((0, api_name.to_string()));
    }
    // Wardrobe name decoding is handled through coded text directly.
    // Using decode_text on arbitrary hashes can trip GW2 text assertions.
    if link_type == encoder::LinkType::Wardrobe {
        let off = 0x68usize;
        if is_readable(ptr, off + std::mem::size_of::<u32>()) {
            let text_hash = (ptr.add(off) as *const u32).read_unaligned();
            if text_hash != 0 {
                for _ in 0..WARDROBE_NAME_RESOLVE_ATTEMPTS {
                    let coded_ptr = resolve_coded_text_ptr(state, text_hash, prop_ctx);
                    if !coded_ptr.is_null() {
                        if let Some(raw) = read_wide_string(coded_ptr, 1024) {
                            let name = raw.trim().to_string();
                            let unresolved = name.is_empty()
                                || name.chars().all(|c| c == '?' || c.is_whitespace());
                            if !unresolved {
                                return (text_hash, name);
                            }
                        }
                    }
                    std::thread::yield_now();
                }
            }
            return (text_hash, String::new());
        }
        return (0, String::new());
    }

    if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
        return (0, api_name.to_string());
    }
    let ty = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    if ty != content_type {
        return (0, api_name.to_string());
    }

    let type_name = link_type.name().to_string();

    let offset = known_name_hash_offset_for_link_type(link_type)
        .or_else(|| state.discovered_name_hash_offsets.get(&type_name).copied())
        .or_else(|| {
            let discovered = unsafe {
                discover_name_hash_offset_for_link_type(
                    state,
                    link_type,
                    content_type,
                    content_ctx,
                    get_content_by_index,
                    count_defs,
                    iterate_content_defs,
                    prop_ctx,
                )
            };
            if let Some(off) = discovered {
                state
                    .discovered_name_hash_offsets
                    .insert(type_name.clone(), off);
            }
            discovered
        });

    if let Some(off) = offset {
        if let Some((hash, name)) = decode_text_hash_at_offset(state, ptr, off, prop_ctx) {
            return (hash, name);
        }
    }

    (0, api_name.to_string())
}

unsafe fn decode_coded_text(
    state: &State,
    coded_text: *const u16,
    prop_ctx: *const u8,
) -> Option<String> {
    if coded_text.is_null() || state.pointers.decode_text_fn.is_null() {
        return None;
    }
    if !is_executable(state.pointers.decode_text_fn as *const u8) {
        return None;
    }
    // The game decoder is sensitive to malformed CodedText pointers. Validate
    // that the input at least looks like a readable, terminated UTF-16 buffer
    // before handing it back to GW2 code.
    wide_string_len_checked(coded_text, 2048)?;

    type TextCallback = unsafe extern "C" fn(usize, *const u16);
    type DecodeTextFn = unsafe extern "C" fn(*const u16, TextCallback, usize);

    let decode: DecodeTextFn = std::mem::transmute(state.pointers.decode_text_fn);

    {
        let mut out = DECODED_TEXT.lock();
        *out = None;
    }

    let run_decode = || decode(coded_text, decode_receiver, 0);

    if !state.pointers.prop_ctx_getter.is_null() {
        let _ = with_prop_ctx_installed(state.pointers.prop_ctx_getter, prop_ctx, run_decode);
    } else {
        run_decode();
    }

    DECODED_TEXT.lock().clone()
}

unsafe fn decode_text_hash(state: &State, text_hash: u32, prop_ctx: *const u8) -> Option<String> {
    if !is_plausible_text_hash(text_hash) {
        return None;
    }
    let coded = resolve_coded_text_ptr(state, text_hash, prop_ctx);
    if coded.is_null() {
        return None;
    }

    decode_coded_text(state, coded, prop_ctx)
}

unsafe fn resolve_coded_text_ptr(state: &State, text_hash: u32, prop_ctx: *const u8) -> *const u16 {
    if !is_plausible_text_hash(text_hash) || state.pointers.resolve_text_hash_fn.is_null() {
        return std::ptr::null();
    }
    if !is_executable(state.pointers.resolve_text_hash_fn as *const u8) {
        return std::ptr::null();
    }

    type ResolveTextHashFn = unsafe extern "C" fn(u32, u32) -> *const u16;
    let resolve: ResolveTextHashFn = std::mem::transmute(state.pointers.resolve_text_hash_fn);
    let run_resolve = || resolve(text_hash, 0);
    if !state.pointers.prop_ctx_getter.is_null() {
        with_prop_ctx_installed(state.pointers.prop_ctx_getter, prop_ctx, run_resolve)
            .unwrap_or(std::ptr::null())
    } else {
        run_resolve()
    }
}

unsafe fn wide_string_len_checked(ptr: *const u16, max_len: usize) -> Option<usize> {
    if ptr.is_null() || max_len == 0 {
        return None;
    }

    for len in 0..max_len {
        let ch_ptr = ptr.add(len);
        if !is_readable(ch_ptr as *const u8, std::mem::size_of::<u16>()) {
            return None;
        }
        if *ch_ptr == 0 {
            return Some(len);
        }
    }

    None
}

unsafe fn read_wide_string(ptr: *const u16, max_len: usize) -> Option<String> {
    if ptr.is_null() || max_len == 0 {
        return None;
    }
    let len = wide_string_len_checked(ptr, max_len)?;
    if len == 0 {
        return Some(String::new());
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf16_lossy(slice))
}

unsafe extern "C" fn decode_receiver(_ctx: usize, decoded_text: *const u16) {
    if decoded_text.is_null() {
        return;
    }

    let Some(len) = wide_string_len_checked(decoded_text, 2048) else {
        return;
    };
    if len == 0 {
        return;
    }

    let slice = std::slice::from_raw_parts(decoded_text, len);
    let text = String::from_utf16_lossy(slice);
    *DECODED_TEXT.lock() = Some(text);
}

fn ensure_scan_pointers(state: &mut State) {
    let module = game_module_range();

    if state.pointers.prop_ctx_getter.is_null() {
        state.pointers.prop_ctx_getter = unsafe {
            scan_raw_first(PROP_CTX_PATTERN, module)
                .or_else(|| scan_raw_first(PROP_CTX_PATTERN_FALLBACK, module))
                .unwrap_or(std::ptr::null_mut())
        };
    }

    if state.pointers.main_thread_match.is_null() {
        state.pointers.main_thread_match =
            unsafe { scan_raw_first(MAIN_THREAD_PATTERN, module).unwrap_or(std::ptr::null_mut()) };
    }

    if state.pointers.resolve_text_hash_fn.is_null() {
        state.pointers.resolve_text_hash_fn = unsafe {
            scan_raw_first(RESOLVE_TEXT_HASH_PATTERN, module).unwrap_or(std::ptr::null_mut())
        };
    }

    if state.pointers.decode_text_fn.is_null() {
        state.pointers.decode_text_fn =
            unsafe { scan_raw_first(DECODE_TEXT_PATTERN, module).unwrap_or(std::ptr::null_mut()) };
    }
}

unsafe fn suggest_end_id_from_content_count(state: &State) -> u32 {
    let Some(prop_ctx) = read_prop_ctx(
        state.pointers.prop_ctx_getter,
        resolve_main_thread_id(state.pointers.main_thread_match),
    ) else {
        return 0;
    };

    let Some(content_ctx) = read_ptr(prop_ctx, PROP_CTX_CONTENT_CTX) else {
        return 0;
    };

    let Some((_, count_defs, _)) = resolve_content_vfuncs(content_ctx) else {
        return 0;
    };

    count_defs(content_ctx, ITEM_CONTENT_TYPE) as u32
}

type GetContentByIndexFn = unsafe extern "C" fn(*const u8, u32, u32) -> *const u8;
type CountContentDefsFn = unsafe extern "C" fn(*const u8, u32) -> i32;
type IterateContentDefsFn = unsafe extern "C" fn(*const u8, u32, *mut u32) -> *const u8;

unsafe fn resolve_content_vfuncs(
    content_ctx: *const u8,
) -> Option<(
    GetContentByIndexFn,
    CountContentDefsFn,
    IterateContentDefsFn,
)> {
    let vtable = read_ptr(content_ctx, 0)?;

    let get_slot = vtable.add(CONTENT_VT_GET_CONTENT_BY_INDEX);
    let count_slot = vtable.add(CONTENT_VT_COUNT_CONTENT_DEFS);
    let iterate_slot = vtable.add(CONTENT_VT_ITERATE_CONTENT_DEFS);

    if !is_readable(get_slot, std::mem::size_of::<usize>())
        || !is_readable(count_slot, std::mem::size_of::<usize>())
        || !is_readable(iterate_slot, std::mem::size_of::<usize>())
    {
        return None;
    }

    let get_addr = *(get_slot as *const usize);
    let count_addr = *(count_slot as *const usize);
    let iterate_addr = *(iterate_slot as *const usize);

    if get_addr == 0 || count_addr == 0 || iterate_addr == 0 {
        return None;
    }

    let get_fn: GetContentByIndexFn = std::mem::transmute(get_addr);
    let count_fn: CountContentDefsFn = std::mem::transmute(count_addr);
    let iterate_fn: IterateContentDefsFn = std::mem::transmute(iterate_addr);

    Some((get_fn, count_fn, iterate_fn))
}

unsafe fn find_content_ptr_by_id_fallback(
    content_ctx: *const u8,
    content_type: u32,
    target_id: u32,
    discovered_id_offset: Option<usize>,
    allow_index_match: bool,
    iterate_content_defs: IterateContentDefsFn,
    count_defs: CountContentDefsFn,
) -> *const u8 {
    let count = count_defs(content_ctx, content_type).max(0) as u32;
    if count == 0 {
        return std::ptr::null();
    }

    let limit = count.min(250_000);
    let mut iter_index: u32 = 0;
    for _ in 0..limit {
        let ptr = iterate_content_defs(content_ctx, content_type, &mut iter_index as *mut u32);
        if ptr.is_null() {
            break;
        }
        if !is_readable(ptr, 0x2C) {
            continue;
        }
        let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != content_type {
            continue;
        }
        let c_index = (ptr.add(CONTENT_DEF_INDEX) as *const u32).read_unaligned();
        let c_id = (ptr.add(0x28) as *const u32).read_unaligned();
        let discovered_match = if let Some(off) = discovered_id_offset {
            if is_readable(ptr, off + std::mem::size_of::<u32>()) {
                (ptr.add(off) as *const u32).read_unaligned() == target_id
            } else {
                false
            }
        } else {
            false
        };
        if (allow_index_match && c_index == target_id) || c_id == target_id || discovered_match {
            return ptr;
        }
    }
    std::ptr::null()
}

unsafe fn discover_id_offset_for_link_type(
    content_ctx: *const u8,
    content_type: u32,
    iterate_content_defs: IterateContentDefsFn,
    count_defs: CountContentDefsFn,
    api_ids: &HashSet<u32>,
) -> Option<usize> {
    if api_ids.is_empty() {
        return None;
    }

    const CANDIDATE_OFFSETS: &[usize] = &[
        0x20, 0x24, 0x28, 0x2C, 0x30, 0x34, 0x38, 0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58,
        0x5C, 0x60, 0x64, 0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C, 0x80,
    ];

    let count = count_defs(content_ctx, content_type).max(0) as u32;
    if count == 0 {
        return None;
    }

    let mut scores: HashMap<usize, usize> = HashMap::new();
    let limit = count.min(120_000);
    let mut iter_index: u32 = 0;
    for _ in 0..limit {
        let ptr = iterate_content_defs(content_ctx, content_type, &mut iter_index as *mut u32);
        if ptr.is_null() {
            break;
        }
        if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
            continue;
        }
        let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != content_type {
            continue;
        }

        for &off in CANDIDATE_OFFSETS {
            if !is_readable(ptr, off + std::mem::size_of::<u32>()) {
                continue;
            }
            let v = (ptr.add(off) as *const u32).read_unaligned();
            if api_ids.contains(&v) {
                *scores.entry(off).or_insert(0) += 1;
            }
        }
    }

    let mut best: Option<(usize, usize)> = None;
    for (off, score) in scores {
        let better = match best {
            Some((best_off, best_score)) => {
                score > best_score || (score == best_score && off == 0x28 && best_off != 0x28)
            }
            None => true,
        };
        if better {
            best = Some((off, score));
        }
    }

    let (off, score) = best?;
    if score >= 3 {
        Some(off)
    } else {
        None
    }
}

unsafe fn resolve_text_hash_at_offset_raw(
    state: &State,
    ptr: *const u8,
    offset: usize,
    prop_ctx: *const u8,
) -> Option<(u32, String)> {
    if !is_readable(ptr, offset + std::mem::size_of::<u32>()) {
        return None;
    }
    let text_hash = (ptr.add(offset) as *const u32).read_unaligned();
    if !is_plausible_text_hash(text_hash) {
        return None;
    }
    let coded_ptr = resolve_coded_text_ptr(state, text_hash, prop_ctx);
    if coded_ptr.is_null() {
        return None;
    }
    let text = read_wide_string(coded_ptr, 512)?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some((text_hash, text.to_string()))
}

unsafe fn find_content_ptr_by_name_fallback(
    state: &State,
    content_ctx: *const u8,
    content_type: u32,
    api_name: &str,
    name_offset: usize,
    iterate_content_defs: IterateContentDefsFn,
    count_defs: CountContentDefsFn,
    prop_ctx: *const u8,
) -> *const u8 {
    let target = normalize_api_name_for_match(api_name);
    if target.is_empty() {
        return std::ptr::null();
    }

    let count = count_defs(content_ctx, content_type).max(0) as u32;
    if count == 0 {
        return std::ptr::null();
    }

    let limit = count.min(250_000);
    let mut iter_index: u32 = 0;
    for _ in 0..limit {
        let ptr = iterate_content_defs(content_ctx, content_type, &mut iter_index as *mut u32);
        if ptr.is_null() {
            break;
        }
        if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
            continue;
        }
        let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != content_type {
            continue;
        }

        let Some((_, resolved)) =
            resolve_text_hash_at_offset_raw(state, ptr, name_offset, prop_ctx)
        else {
            continue;
        };

        let resolved_norm = normalize_decoded_name_for_match(&resolved);
        if resolved_norm == target {
            return ptr;
        }
    }
    std::ptr::null()
}

unsafe fn find_content_ptr_by_any_u32_match(
    content_ctx: *const u8,
    content_type: u32,
    target_id: u32,
    iterate_content_defs: IterateContentDefsFn,
    count_defs: CountContentDefsFn,
) -> *const u8 {
    const CANDIDATE_OFFSETS: &[usize] = &[
        0x20, 0x24, 0x28, 0x2C, 0x30, 0x34, 0x38, 0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58,
        0x5C, 0x60, 0x64, 0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C, 0x80, 0x84, 0x88, 0x8C, 0x90, 0x94,
        0x98, 0x9C, 0xA0, 0xA4, 0xA8, 0xAC, 0xB0, 0xB4, 0xB8, 0xBC, 0xC0,
    ];

    let count = count_defs(content_ctx, content_type).max(0) as u32;
    if count == 0 {
        return std::ptr::null();
    }

    let limit = count.min(150_000);
    let mut iter_index: u32 = 0;
    for _ in 0..limit {
        let ptr = iterate_content_defs(content_ctx, content_type, &mut iter_index as *mut u32);
        if ptr.is_null() {
            break;
        }
        if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
            continue;
        }
        let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != content_type {
            continue;
        }
        for &off in CANDIDATE_OFFSETS {
            if !is_readable(ptr, off + std::mem::size_of::<u32>()) {
                continue;
            }
            let v = (ptr.add(off) as *const u32).read_unaligned();
            if v == target_id {
                return ptr;
            }
        }
    }
    std::ptr::null()
}

unsafe fn find_any_type_content_ptr_for_id(
    content_ctx: *const u8,
    target_id: u32,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
) -> Option<(u32, *const u8)> {
    for ty in 0u32..=478u32 {
        let count = count_defs(content_ctx, ty).max(0) as u32;
        if count == 0 {
            continue;
        }
        let ptr = find_content_ptr_by_any_u32_match(
            content_ctx,
            ty,
            target_id,
            iterate_content_defs,
            count_defs,
        );
        if !ptr.is_null() {
            return Some((ty, ptr));
        }
    }
    None
}

unsafe fn content_ptr_matches_type_and_id(ptr: *const u8, content_type: u32, id: u32) -> bool {
    if ptr.is_null() || !is_readable(ptr, 0x2C) {
        return false;
    }
    let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
    let c_id = (ptr.add(0x28) as *const u32).read_unaligned();
    c_type == content_type && c_id == id
}

unsafe fn get_strict_content_ptr_for_debug(
    state: &mut State,
    link_type: encoder::LinkType,
    content_ctx: *const u8,
    content_type: u32,
    id: u32,
    api_name: Option<&str>,
    get_content_by_index: GetContentByIndexFn,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
    prop_ctx: *const u8,
) -> *const u8 {
    let _ = (api_name, prop_ctx);

    // Debug lookup must be strict: never accept index-based matches.
    let by_index = get_content_by_index(content_ctx, content_type, id);
    if content_ptr_matches_type_and_id(by_index, content_type, id) {
        return by_index;
    }

    let type_name = link_type.name().to_string();
    let discovered_id_offset = state
        .discovered_id_offsets
        .get(&type_name)
        .copied()
        .or_else(|| {
            let ids: HashSet<u32> = load_api_id_name_pairs_for_link_type(link_type)
                .into_iter()
                .map(|(candidate_id, _)| candidate_id)
                .collect();
            let found = discover_id_offset_for_link_type(
                content_ctx,
                content_type,
                iterate_content_defs,
                count_defs,
                &ids,
            );
            if let Some(off) = found {
                state.discovered_id_offsets.insert(type_name, off);
            }
            found
        });

    let by_id = find_content_ptr_by_id_fallback(
        content_ctx,
        content_type,
        id,
        discovered_id_offset,
        false,
        iterate_content_defs,
        count_defs,
    );
    if content_ptr_matches_type_and_id(by_id, content_type, id) {
        return by_id;
    }

    std::ptr::null()
}

unsafe fn discover_name_offset_from_scan(
    state: &State,
    content_ctx: *const u8,
    content_type: u32,
    iterate_content_defs: IterateContentDefsFn,
    count_defs: CountContentDefsFn,
    api_names: &HashSet<String>,
    prop_ctx: *const u8,
) -> Option<usize> {
    if api_names.is_empty() {
        return None;
    }

    const CANDIDATE_OFFSETS: &[usize] = &[
        0x2C, 0x30, 0x34, 0x38, 0x3C, 0x40, 0x44, 0x48, 0x4C, 0x50, 0x54, 0x58, 0x5C, 0x60, 0x64,
        0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C, 0x80, 0x84,
    ];

    let count = count_defs(content_ctx, content_type).max(0) as u32;
    if count == 0 {
        return None;
    }

    let sample_limit = count.min(6_000);
    let mut ptrs: Vec<*const u8> = Vec::new();
    let mut iter_index: u32 = 0;
    for _ in 0..sample_limit {
        let ptr = iterate_content_defs(content_ctx, content_type, &mut iter_index as *mut u32);
        if ptr.is_null() {
            break;
        }
        if !is_readable(ptr, CONTENT_DEF_TYPE + std::mem::size_of::<u32>()) {
            continue;
        }
        let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
        if c_type != content_type {
            continue;
        }
        ptrs.push(ptr);
        if ptrs.len() >= 512 {
            break;
        }
    }
    if ptrs.len() < 8 {
        return None;
    }

    let mut best: Option<(usize, usize, usize)> = None; // (offset, matches, tested)
    for &off in CANDIDATE_OFFSETS {
        let mut matches = 0usize;
        let mut tested = 0usize;
        for &ptr in ptrs.iter().take(256) {
            let Some((_, resolved)) = resolve_text_hash_at_offset_raw(state, ptr, off, prop_ctx)
            else {
                continue;
            };
            tested += 1;
            let norm = normalize_decoded_name_for_match(&resolved);
            if api_names.contains(&norm) {
                matches += 1;
            }
        }
        if tested == 0 {
            continue;
        }
        let better = match best {
            Some((best_off, best_matches, best_tested)) => {
                matches > best_matches
                    || (matches == best_matches && tested > best_tested)
                    || (matches == best_matches
                        && tested == best_tested
                        && off == 0x64
                        && best_off != 0x64)
            }
            None => true,
        };
        if better {
            best = Some((off, matches, tested));
        }
    }

    let (off, matches, tested) = best?;
    if matches >= 4 && matches * 3 >= tested {
        Some(off)
    } else {
        None
    }
}

unsafe fn get_content_ptr_for_type_id(
    state: &mut State,
    link_type: encoder::LinkType,
    content_ctx: *const u8,
    content_type: u32,
    id: u32,
    api_name: Option<&str>,
    get_content_by_index: GetContentByIndexFn,
    count_defs: CountContentDefsFn,
    iterate_content_defs: IterateContentDefsFn,
    prop_ctx: *const u8,
    allow_deep_scan: bool,
) -> *const u8 {
    let type_name = link_type.name().to_string();
    let allow_probe_fallbacks = link_type == encoder::LinkType::Item;
    let allow_index_match = link_type == encoder::LinkType::Item;
    let ptr = get_content_by_index(content_ctx, content_type, id);
    if !ptr.is_null() {
        if is_readable(ptr, 0x2C + std::mem::size_of::<u32>()) {
            let c_type = (ptr.add(CONTENT_DEF_TYPE) as *const u32).read_unaligned();
            let c_id = (ptr.add(0x28) as *const u32).read_unaligned();
            if c_type == content_type && (allow_index_match || c_id == id) {
                return ptr;
            }
        }
    }
    let discovered_id_offset = state
        .discovered_id_offsets
        .get(&type_name)
        .copied()
        .or_else(|| {
            let ids: HashSet<u32> = load_api_id_name_pairs_for_link_type(link_type)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let found = unsafe {
                discover_id_offset_for_link_type(
                    content_ctx,
                    content_type,
                    iterate_content_defs,
                    count_defs,
                    &ids,
                )
            };
            if let Some(off) = found {
                state.discovered_id_offsets.insert(type_name.clone(), off);
            }
            found
        });

    let by_id = find_content_ptr_by_id_fallback(
        content_ctx,
        content_type,
        id,
        discovered_id_offset,
        allow_index_match,
        iterate_content_defs,
        count_defs,
    );
    if !by_id.is_null() {
        return by_id;
    }

    // Non-item types use only base id scanning (no name/deep probing fallbacks).
    if !allow_probe_fallbacks {
        return std::ptr::null();
    }

    let name_offset = known_name_hash_offset_for_link_type(link_type)
        .or_else(|| {
            state
                .discovered_name_hash_offsets
                .get(link_type.name())
                .copied()
        })
        .or_else(|| {
            let api_names: HashSet<String> = load_api_id_name_pairs_for_link_type(link_type)
                .into_iter()
                .map(|(_, n)| normalize_api_name_for_match(&n))
                .filter(|n| !n.is_empty())
                .collect();
            let found = unsafe {
                discover_name_offset_from_scan(
                    state,
                    content_ctx,
                    content_type,
                    iterate_content_defs,
                    count_defs,
                    &api_names,
                    prop_ctx,
                )
            };
            if let Some(off) = found {
                state
                    .discovered_name_hash_offsets
                    .insert(type_name.clone(), off);
            }
            found
        });
    if let (Some(api_name), Some(name_offset)) = (api_name, name_offset) {
        let by_name = find_content_ptr_by_name_fallback(
            state,
            content_ctx,
            content_type,
            api_name,
            name_offset,
            iterate_content_defs,
            count_defs,
            prop_ctx,
        );
        if !by_name.is_null() {
            return by_name;
        }
    }

    if allow_deep_scan {
        let by_any = find_content_ptr_by_any_u32_match(
            content_ctx,
            content_type,
            id,
            iterate_content_defs,
            count_defs,
        );
        if !by_any.is_null() {
            return by_any;
        }
    }

    std::ptr::null()
}

fn load_api_index() -> (HashSet<u32>, HashMap<u32, String>) {
    match load_cache::<Vec<db::items::Item>>(API_ITEMS_CACHE_FILE) {
        Some(items) => {
            let mut ids = HashSet::with_capacity(items.len());
            let mut names = HashMap::with_capacity(items.len());
            for item in items {
                ids.insert(item.id);
                names.insert(item.id, item.name);
            }
            (ids, names)
        }
        None => (HashSet::new(), HashMap::new()),
    }
}

fn cache_path(file: &str) -> Option<PathBuf> {
    nexus::paths::get_addon_dir("chat_link_generator").map(|d| d.join(file))
}

fn load_cache<T: for<'de> Deserialize<'de>>(file: &str) -> Option<T> {
    let path = cache_path(file)?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_cache<T: Serialize>(file: &str, value: &T) {
    let Some(path) = cache_path(file) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(value) {
        let _ = std::fs::write(path, json);
    }
}

fn delete_cache_local(file: &str) {
    let Some(path) = cache_path(file) else {
        return;
    };
    let _ = std::fs::remove_file(path);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "windows")]
unsafe fn scan_raw_first(pattern_str: &str, module: Option<(usize, usize)>) -> Option<*mut u8> {
    let pattern = Pattern::from_str(pattern_str).ok()?;
    let scan = PatternScan::new(pattern, vec![]);
    let matches: Vec<*mut u8> = scan.scan_all::<u8>();
    for &ptr in &matches {
        if let Some((ms, me)) = module {
            let addr = ptr as usize;
            if addr < ms || addr >= me {
                continue;
            }
        }
        return Some(ptr);
    }
    None
}

#[cfg(not(target_os = "windows"))]
unsafe fn scan_raw_first(_pattern_str: &str, _module: Option<(usize, usize)>) -> Option<*mut u8> {
    None
}

#[cfg(target_os = "windows")]
fn game_module_range() -> Option<(usize, usize)> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    unsafe {
        let handle = GetModuleHandleW(None).ok()?;
        let base = handle.0 as *const u8;
        if !is_readable(base, 0x40) {
            return None;
        }
        let e_lfanew = *(base.add(0x3C) as *const i32) as usize;
        let pe = base.add(e_lfanew);
        if !is_readable(pe, 0x78) {
            return None;
        }
        let size = *(pe.add(4 + 20 + 56) as *const u32) as usize;
        let start = base as usize;
        Some((start, start + size))
    }
}

#[cfg(not(target_os = "windows"))]
fn game_module_range() -> Option<(usize, usize)> {
    None
}

#[cfg(target_os = "windows")]
unsafe fn read_ptr(base: *const u8, offset: usize) -> Option<*const u8> {
    let slot = base.add(offset);
    if !is_readable(slot, std::mem::size_of::<usize>()) {
        return None;
    }
    let ptr = *(slot as *const *const u8);
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

#[cfg(not(target_os = "windows"))]
unsafe fn read_ptr(_base: *const u8, _offset: usize) -> Option<*const u8> {
    None
}

#[cfg(target_os = "windows")]
unsafe fn resolve_main_thread_id(match_addr: *mut u8) -> Option<u32> {
    if match_addr.is_null() || !is_readable(match_addr as *const u8, 11) {
        return None;
    }
    let disp = (match_addr.add(7) as *const i32).read_unaligned();
    let global_addr = match_addr.add(11).offset(disp as isize);
    if !is_readable(global_addr as *const u8, 4) {
        return None;
    }
    let tid = *(global_addr as *const u32);
    if tid == 0 {
        None
    } else {
        Some(tid)
    }
}

#[cfg(not(target_os = "windows"))]
unsafe fn resolve_main_thread_id(_match_addr: *mut u8) -> Option<u32> {
    None
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
unsafe fn read_prop_ctx(fn_ptr: *mut u8, main_thread_id: Option<u32>) -> Option<*const u8> {
    if fn_ptr.is_null() || !is_readable(fn_ptr as *const u8, 30) {
        return None;
    }

    let b0 = *fn_ptr;
    let b1 = *fn_ptr.add(1);
    if b0 != 0x8B || (b1 != 0x0D && b1 != 0x15) {
        return None;
    }

    let disp = (fn_ptr.add(2) as *const i32).read_unaligned();
    let tls_index_ptr = fn_ptr.add(6).offset(disp as isize) as *const u32;
    if !is_readable(tls_index_ptr as *const u8, 4) {
        return None;
    }
    let tls_index = *tls_index_ptr as usize;

    let tls_offset = if *fn_ptr.add(15) == 0xBA {
        (fn_ptr.add(16) as *const u32).read_unaligned() as usize
    } else if *fn_ptr.add(15) == 0x41 && *fn_ptr.add(16) == 0xB8 {
        (fn_ptr.add(17) as *const u32).read_unaligned() as usize
    } else {
        return None;
    };

    let tls_array: *const *const u8 = if let Some(tid) = main_thread_id {
        read_teb_tls_array_of_thread(tid)?
    } else {
        let ptr: *const *const u8;
        core::arch::asm!("mov {}, gs:[0x58]", out(reg) ptr, options(nostack, preserves_flags));
        ptr
    };

    if tls_array.is_null() {
        return None;
    }

    let tls_entry = tls_array.add(tls_index) as *const u8;
    if !is_readable(tls_entry, std::mem::size_of::<usize>()) {
        return None;
    }

    let tls_block = *tls_array.add(tls_index);
    if tls_block.is_null() {
        return None;
    }

    let prop_ctx_slot = tls_block.add(tls_offset);
    if !is_readable(prop_ctx_slot, std::mem::size_of::<usize>()) {
        return None;
    }

    let prop_ctx = *(prop_ctx_slot as *const *const u8);
    if prop_ctx.is_null() || !is_readable(prop_ctx, 0x300) {
        None
    } else {
        Some(prop_ctx)
    }
}

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
unsafe fn read_prop_ctx(_fn_ptr: *mut u8, _main_thread_id: Option<u32>) -> Option<*const u8> {
    None
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
unsafe fn with_prop_ctx_installed<R>(
    fn_ptr: *mut u8,
    prop_ctx: *const u8,
    f: impl FnOnce() -> R,
) -> Option<R> {
    if fn_ptr.is_null() || !is_readable(fn_ptr as *const u8, 30) || prop_ctx.is_null() {
        return None;
    }

    let b0 = *fn_ptr;
    let b1 = *fn_ptr.add(1);
    if b0 != 0x8B || (b1 != 0x0D && b1 != 0x15) {
        return None;
    }

    let disp = (fn_ptr.add(2) as *const i32).read_unaligned();
    let tls_index_ptr = fn_ptr.add(6).offset(disp as isize) as *const u32;
    if !is_readable(tls_index_ptr as *const u8, 4) {
        return None;
    }
    let tls_index = *tls_index_ptr as usize;

    let tls_offset = if *fn_ptr.add(15) == 0xBA {
        (fn_ptr.add(16) as *const u32).read_unaligned() as usize
    } else if *fn_ptr.add(15) == 0x41 && *fn_ptr.add(16) == 0xB8 {
        (fn_ptr.add(17) as *const u32).read_unaligned() as usize
    } else {
        return None;
    };

    let tls_array: *const *mut u8;
    core::arch::asm!("mov {}, gs:[0x58]", out(reg) tls_array, options(nostack, preserves_flags));
    if tls_array.is_null() {
        return None;
    }

    let tls_entry = tls_array.add(tls_index) as *const u8;
    if !is_readable(tls_entry, std::mem::size_of::<usize>()) {
        return None;
    }

    let tls_block = *tls_array.add(tls_index);
    if tls_block.is_null() {
        return None;
    }

    let slot_ptr = tls_block.add(tls_offset);
    if !is_readable(slot_ptr, std::mem::size_of::<usize>())
        || !is_writable(slot_ptr, std::mem::size_of::<usize>())
    {
        return None;
    }

    let slot = slot_ptr as *mut *const u8;
    let original = *slot;
    struct TlsRestore {
        slot: *mut *const u8,
        original: *const u8,
    }
    impl Drop for TlsRestore {
        fn drop(&mut self) {
            unsafe {
                *self.slot = self.original;
            }
        }
    }

    *slot = prop_ctx;
    let _restore = TlsRestore { slot, original };
    let out = f();
    Some(out)
}

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
unsafe fn with_prop_ctx_installed<R>(
    _fn_ptr: *mut u8,
    _prop_ctx: *const u8,
    _f: impl FnOnce() -> R,
) -> Option<R> {
    None
}

#[cfg(target_os = "windows")]
unsafe fn read_teb_tls_array_of_thread(thread_id: u32) -> Option<*const *const u8> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows::Win32::System::Threading::{OpenThread, THREAD_QUERY_INFORMATION};

    let handle = OpenThread(THREAD_QUERY_INFORMATION, false, thread_id).ok()?;
    let ntdll = GetModuleHandleA(windows::core::s!("ntdll.dll")).ok()?;
    let proc = GetProcAddress(ntdll, windows::core::s!("NtQueryInformationThread"))?;

    type NtQueryInfoThread = unsafe extern "system" fn(
        handle: *mut core::ffi::c_void,
        class: u32,
        info: *mut u8,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;

    let nt_query: NtQueryInfoThread = std::mem::transmute(proc);
    let mut info = [0u8; 0x30];
    let status = nt_query(handle.0, 0, info.as_mut_ptr(), 0x30, std::ptr::null_mut());
    let _ = CloseHandle(handle);

    if status != 0 {
        return None;
    }

    let teb = *(info.as_ptr().add(0x08) as *const *const u8);
    if teb.is_null() || !is_readable(teb, 0x60) {
        return None;
    }

    if !is_readable(teb.add(0x58), std::mem::size_of::<usize>()) {
        return None;
    }

    let tls_array = *(teb.add(0x58) as *const *const *const u8);
    if tls_array.is_null() {
        None
    } else {
        Some(tls_array)
    }
}

#[cfg(not(target_os = "windows"))]
unsafe fn read_teb_tls_array_of_thread(_thread_id: u32) -> Option<*const *const u8> {
    None
}

#[cfg(target_os = "windows")]
unsafe fn is_readable(ptr: *const u8, size: usize) -> bool {
    use windows::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS,
    };

    if ptr.is_null() {
        return false;
    }

    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = VirtualQuery(
        Some(ptr as *const _),
        &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );

    if ret == 0 {
        return false;
    }

    if mbi.State != MEM_COMMIT {
        return false;
    }

    let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
    let Some(read_end) = (ptr as usize).checked_add(size) else {
        return false;
    };
    if read_end > region_end {
        return false;
    }

    let protect = mbi.Protect;
    if protect.contains(PAGE_NOACCESS) || protect.contains(PAGE_GUARD) {
        return false;
    }

    mbi.RegionSize > 0
}

#[cfg(not(target_os = "windows"))]
unsafe fn is_readable(_ptr: *const u8, _size: usize) -> bool {
    false
}

#[cfg(target_os = "windows")]
unsafe fn is_writable(ptr: *const u8, size: usize) -> bool {
    use windows::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE, PAGE_WRITECOPY,
    };

    if ptr.is_null() {
        return false;
    }

    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = VirtualQuery(
        Some(ptr as *const _),
        &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if ret == 0 || mbi.State != MEM_COMMIT {
        return false;
    }

    let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
    let Some(write_end) = (ptr as usize).checked_add(size) else {
        return false;
    };
    if write_end > region_end {
        return false;
    }

    let protect = mbi.Protect;
    if protect.contains(PAGE_NOACCESS) || protect.contains(PAGE_GUARD) {
        return false;
    }

    protect.contains(PAGE_READWRITE)
        || protect.contains(PAGE_WRITECOPY)
        || protect.contains(PAGE_EXECUTE_READWRITE)
        || protect.contains(PAGE_EXECUTE_WRITECOPY)
}

#[cfg(not(target_os = "windows"))]
unsafe fn is_writable(_ptr: *const u8, _size: usize) -> bool {
    false
}

#[cfg(target_os = "windows")]
unsafe fn is_executable(ptr: *const u8) -> bool {
    use windows::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE, PAGE_EXECUTE_READ,
        PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS,
    };

    if ptr.is_null() {
        return false;
    }

    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = VirtualQuery(
        Some(ptr as *const _),
        &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if ret == 0 || mbi.State != MEM_COMMIT {
        return false;
    }

    let protect = mbi.Protect;
    if protect.contains(PAGE_NOACCESS) || protect.contains(PAGE_GUARD) {
        return false;
    }

    protect.contains(PAGE_EXECUTE)
        || protect.contains(PAGE_EXECUTE_READ)
        || protect.contains(PAGE_EXECUTE_READWRITE)
        || protect.contains(PAGE_EXECUTE_WRITECOPY)
}

#[cfg(not(target_os = "windows"))]
unsafe fn is_executable(_ptr: *const u8) -> bool {
    false
}
