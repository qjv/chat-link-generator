use std::borrow::Cow;
use std::collections::BTreeMap;

use nexus::imgui::{
    Condition, Drag, InputText, InputTextFlags, MouseButton, Selectable, StyleColor, TableFlags,
    Ui, Window,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::config::RUNTIME_CONFIG;
use crate::db::items::{ItemFilter, UpgradeFilter};
use crate::db::{self, DbStatus};
use crate::encoder::{self, LinkType};

const BATCH_COLORS: &[[f32; 4]] = &[
    [0.60, 0.40, 0.80, 1.0],
    [0.30, 0.70, 0.50, 1.0],
    [0.80, 0.50, 0.30, 1.0],
    [0.40, 0.60, 0.85, 1.0],
    [0.75, 0.35, 0.55, 1.0],
    [0.35, 0.75, 0.75, 1.0],
    [0.85, 0.65, 0.30, 1.0],
    [0.50, 0.45, 0.80, 1.0],
    [0.65, 0.80, 0.40, 1.0],
    [0.80, 0.40, 0.70, 1.0],
];

const RESULTS_PER_PAGE: usize = 10;

// --- Batch state ---

struct Batch {
    start_id: u32,
    end_id: u32,
    text: String,
    color_index: usize,
}

#[derive(Default)]
struct BatchState {
    batches: Vec<Batch>,
    generated: bool,
    link_type: usize,
}

static BATCH_STATE: Lazy<Mutex<BatchState>> = Lazy::new(|| Mutex::new(BatchState::default()));

// --- Individual link state ---

struct ItemFields {
    id: i32,
    quantity: i32,
    skin_id: i32,
    use_skin: bool,
    upgrade1_id: i32,
    use_upgrade1: bool,
    upgrade2_id: i32,
    use_upgrade2: bool,
    result: String,

    // Search state
    item_search: String,
    item_filter: usize,
    item_min_id: i32,
    item_max_id: i32,
    item_sort_mode: usize,
    item_show_char_count: bool,
    item_results: Vec<(u32, String)>,
    item_page: usize,

    upgrade1_search: String,
    upgrade1_filter: usize,
    upgrade1_results: Vec<(u32, String)>,
    upgrade1_page: usize,

    upgrade2_search: String,
    upgrade2_filter: usize,
    upgrade2_results: Vec<(u32, String)>,
    upgrade2_page: usize,
}

impl Default for ItemFields {
    fn default() -> Self {
        Self {
            id: 1,
            quantity: 1,
            skin_id: 0,
            use_skin: false,
            upgrade1_id: 0,
            use_upgrade1: false,
            upgrade2_id: 0,
            use_upgrade2: false,
            result: String::new(),
            item_search: String::new(),
            item_filter: 0,
            item_min_id: 0,
            item_max_id: 0,
            item_sort_mode: 0,
            item_show_char_count: true,
            item_results: Vec::new(),
            item_page: 0,
            upgrade1_search: String::new(),
            upgrade1_filter: 0,
            upgrade1_results: Vec::new(),
            upgrade1_page: 0,
            upgrade2_search: String::new(),
            upgrade2_filter: 0,
            upgrade2_results: Vec::new(),
            upgrade2_page: 0,
        }
    }
}

/// Searchable fields for non-item types (Skill, Trait, Recipe, Skin, Outfit, Map/POI).
struct SearchableFields {
    id: i32,
    result: String,
    search: String,
    filter_index: usize,
    min_id: i32,
    max_id: i32,
    sort_mode: usize,
    search_results: Vec<(u32, String)>,
    page: usize,
}

impl Default for SearchableFields {
    fn default() -> Self {
        Self {
            id: 1,
            result: String::new(),
            search: String::new(),
            filter_index: 0,
            min_id: 0,
            max_id: 0,
            sort_mode: 0,
            search_results: Vec::new(),
            page: 0,
        }
    }
}

struct IndividualState {
    selected_type: usize,
    prev_selected_type: usize,
    item_fields: ItemFields,
    // One SearchableFields per link type, indexed by LinkType position
    // Index 0=Item (unused here), 1=Map, 2=Skill, 3=Trait, 4=Recipe, 5=Wardrobe, 6=Outfit
    searchable: [SearchableFields; 7],
}

impl IndividualState {
    fn new() -> Self {
        Self {
            selected_type: 0,
            prev_selected_type: 0,
            item_fields: ItemFields::default(),
            searchable: [
                SearchableFields::default(),
                SearchableFields::default(),
                SearchableFields::default(),
                SearchableFields::default(),
                SearchableFields::default(),
                SearchableFields::default(),
                SearchableFields::default(),
            ],
        }
    }
}

static INDIVIDUAL_STATE: Lazy<Mutex<IndividualState>> =
    Lazy::new(|| Mutex::new(IndividualState::new()));

struct InGameDbUiState {
    selected_type: usize,
    search: String,
    api_filter: usize, // 0=All, 1=Present on API, 2=Not on API
    include_api_on_name_parse: bool,
    include_descriptions_on_name_parse: bool,
    min_id: i32,
    max_id: i32,
    sort_mode: usize,
    wardrobe_flag_filter: usize, // 0=All,1=ShowInWardrobe,2=HideIfLocked,3=NoCost,4=OverrideRarity,5=No OverrideRarity
    map_poi_filter: usize,       // 0=All,1=POI,2=Waypoint,3=Unlock,4=Vista,5=Unknown
    show_char_count: bool,
    selected_id: i32,
    results: Vec<db::ingame_items::SearchResult>,
    page: usize,
    last_selected_type: usize,
    last_query: String,
    last_api_filter: usize,
    last_min_id: i32,
    last_max_id: i32,
    last_sort_mode: usize,
    last_wardrobe_flag_filter: usize,
    last_map_poi_filter: usize,
}

impl Default for InGameDbUiState {
    fn default() -> Self {
        Self {
            selected_type: 0,
            search: String::new(),
            api_filter: 0,
            include_api_on_name_parse: false,
            include_descriptions_on_name_parse: false,
            min_id: 0,
            max_id: 0,
            sort_mode: 0,
            wardrobe_flag_filter: 0,
            map_poi_filter: 0,
            show_char_count: true,
            selected_id: 0,
            results: Vec::new(),
            page: 0,
            last_selected_type: 0,
            last_query: String::new(),
            last_api_filter: 0,
            last_min_id: 0,
            last_max_id: 0,
            last_sort_mode: 0,
            last_wardrobe_flag_filter: 0,
            last_map_poi_filter: 0,
        }
    }
}

static INGAME_DB_UI_STATE: Lazy<Mutex<InGameDbUiState>> =
    Lazy::new(|| Mutex::new(InGameDbUiState::default()));

struct AllDataUiState {
    selected_type: usize,
    search: String,
    selected_id: u32,
    page: usize,
    jump_page: i32,
    row_mode: usize, // 0 = merged by id, 1 = non-merged
    cache_type: usize,
    cache_search: String,
    cache_row_mode: usize,
    cache_rows: Vec<AllDataRow>,
    cache_api_count: usize,
    cache_game_count: usize,
    cache_api_db_count: usize,
    cache_api_db_status: DbStatus,
}

#[derive(Clone)]
struct AllDataRow {
    id: u32,
    api_raw: Option<String>,
    game_raw: Option<String>,
    api_norm: String,
    game_norm: String,
    matched: bool,
}

impl Default for AllDataUiState {
    fn default() -> Self {
        Self {
            selected_type: 0,
            search: String::new(),
            selected_id: 0,
            page: 0,
            jump_page: 1,
            row_mode: 0,
            cache_type: usize::MAX,
            cache_search: String::new(),
            cache_row_mode: usize::MAX,
            cache_rows: Vec::new(),
            cache_api_count: 0,
            cache_game_count: 0,
            cache_api_db_count: 0,
            cache_api_db_status: DbStatus::NotLoaded,
        }
    }
}

static ALL_DATA_UI_STATE: Lazy<Mutex<AllDataUiState>> =
    Lazy::new(|| Mutex::new(AllDataUiState::default()));

struct DebugProbeUiState {
    selected_type: usize,
    resolved_candidates: Vec<u32>,
    selected_candidate_idx: usize,
    candidates_for_id: u32,
    use_custom_content_type: bool,
    custom_content_type: i32,
    id: i32,
    selected_offset: i32,
    selected_subdef_offset: i32,
    resolve_from_subdef: bool,
    subdef_mode: bool,
    field_search_value: i32,
    only_matching_fields: bool,
    resolve_pending: bool,
    last_resolve: Option<db::ingame_items::DebugResolveResult>,
    last_resolve_error: String,
    last_error: String,
    last_info: Option<db::ingame_items::ContentProbeDebugInfo>,
}

impl Default for DebugProbeUiState {
    fn default() -> Self {
        Self {
            selected_type: 0,
            resolved_candidates: Vec::new(),
            selected_candidate_idx: 0,
            candidates_for_id: 0,
            use_custom_content_type: false,
            custom_content_type: 0,
            id: 0,
            selected_offset: 0x40,
            selected_subdef_offset: -1,
            resolve_from_subdef: false,
            subdef_mode: false,
            field_search_value: 0,
            only_matching_fields: false,
            resolve_pending: false,
            last_resolve: None,
            last_resolve_error: String::new(),
            last_error: String::new(),
            last_info: None,
        }
    }
}

static DEBUG_PROBE_UI_STATE: Lazy<Mutex<DebugProbeUiState>> =
    Lazy::new(|| Mutex::new(DebugProbeUiState::default()));

fn ingame_item_type_label(code: u32) -> &'static str {
    match code {
        0 => "Armor",
        2 => "Back",
        3 => "Bag",
        4 => "Consumable",
        5 => "Container",
        6 => "CraftingMaterial",
        9 => "Gathering",
        10 => "Gizmo",
        11 => "JadeTechModule",
        12 => "Key",
        15 => "MiniPet",
        17 => "PowerCore",
        18 => "Relic",
        19 => "Tool",
        21 => "Trinket",
        22 => "Trophy",
        23 => "UpgradeComponent",
        24 => "Weapon",
        _ => "Unknown",
    }
}

fn ingame_rarity_label(code: u32) -> &'static str {
    match code {
        0 => "Junk",
        1 => "Basic",
        2 => "Fine",
        3 => "Masterwork",
        4 => "Rare",
        5 => "Exotic",
        6 => "Ascended",
        7 => "Legendary",
        _ => "Unknown",
    }
}

// --- Batch helpers ---

fn generate_batch(
    link_type: LinkType,
    start_id: u32,
    batch_size: u32,
    show_id_prefix: bool,
) -> Batch {
    let end_id = start_id + batch_size - 1;
    let mut parts = Vec::with_capacity(batch_size as usize);
    for id in start_id..=end_id {
        let code = encoder::generate_batch_link(link_type, id);
        if show_id_prefix {
            parts.push(format!("{}{}", id, code));
        } else {
            parts.push(code);
        }
    }
    let color_index = (start_id as usize / batch_size.max(1) as usize) % BATCH_COLORS.len();
    Batch {
        start_id,
        end_id,
        text: parts.join(" "),
        color_index,
    }
}

fn generate_initial_batches(
    link_type_index: usize,
    start_id: i32,
    batch_size: i32,
    show_id_prefix: bool,
) {
    let link_type = LinkType::ALL[link_type_index];
    let start = start_id.max(0) as u32;
    let size = (batch_size.max(1) as u32).min(1000);
    let batch = generate_batch(link_type, start, size, show_id_prefix);
    let mut state = BATCH_STATE.lock();
    state.batches.clear();
    state.batches.push(batch);
    state.generated = true;
    state.link_type = link_type_index;
}

fn append_forward_batch(show_id_prefix: bool) {
    let mut state = BATCH_STATE.lock();
    if state.batches.is_empty() {
        return;
    }
    let last = state.batches.last().unwrap();
    let next_start = last.end_id + 1;
    let batch_size = last.end_id - last.start_id + 1;
    let link_type = LinkType::ALL[state.link_type];
    let batch = generate_batch(link_type, next_start, batch_size, show_id_prefix);
    state.batches.push(batch);
}

fn prepend_backward_batch(show_id_prefix: bool) {
    let mut state = BATCH_STATE.lock();
    if state.batches.is_empty() {
        return;
    }
    let first = state.batches.first().unwrap();
    let batch_size = first.end_id - first.start_id + 1;
    if first.start_id == 0 {
        return;
    }
    let new_start = if first.start_id >= batch_size {
        first.start_id - batch_size
    } else {
        0
    };
    let actual_size = first.start_id - new_start;
    let link_type = LinkType::ALL[state.link_type];
    let batch = generate_batch(link_type, new_start, actual_size, show_id_prefix);
    state.batches.insert(0, batch);
}

// --- Rendering ---

pub fn render_main_window(ui: &Ui) {
    db::ingame_items::maybe_auto_update_on_load();

    let mut open = {
        let cfg = RUNTIME_CONFIG.lock();
        cfg.show_main_window
    };

    if !open {
        return;
    }

    let (status, _, _, _) = db::ingame_items::get_status();
    if matches!(status, DbStatus::Loading | DbStatus::Updating)
        || db::ingame_items::has_pending_debug_resolve()
    {
        db::ingame_items::tick();
    }

    Window::new("Chat Link Generator")
        .size([700.0, 600.0], Condition::FirstUseEver)
        .opened(&mut open)
        .build(ui, || {
            render_tabs(ui);
        });

    {
        let mut cfg = RUNTIME_CONFIG.lock();
        cfg.show_main_window = open;
    }
}

fn render_tabs(ui: &Ui) {
    if let Some(_tab_bar) = ui.tab_bar("##main_tabs") {
        if let Some(_tab) = ui.tab_item("Batch Generator") {
            render_batch_tab(ui);
        }
        if let Some(_tab) = ui.tab_item("API Data") {
            render_individual_tab(ui);
        }
        if let Some(_tab) = ui.tab_item("Game Data") {
            render_ingame_item_db_tab(ui);
        }
        if let Some(_tab) = ui.tab_item("All Data") {
            render_all_data_tab(ui);
        }
        if let Some(_tab) = ui.tab_item("Debug") {
            render_debug_probe_tab(ui);
        }
    }
}

fn render_batch_tab(ui: &Ui) {
    let (mut link_type_index, mut start_id, mut batch_size, mut show_id_prefix) = {
        let cfg = RUNTIME_CONFIG.lock();
        (
            cfg.link_type_index,
            cfg.start_id,
            cfg.batch_size,
            cfg.show_id_prefix,
        )
    };

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    let prev_type = link_type_index;
    ui.set_next_item_width(180.0);
    if ui.combo("Type", &mut link_type_index, &type_names, |name| {
        Cow::Borrowed(name)
    }) {
        if link_type_index != prev_type {
            start_id = LinkType::ALL[link_type_index].default_start() as i32;
        }
    }

    ui.same_line();
    ui.set_next_item_width(100.0);
    Drag::new("Start ID")
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut start_id);

    ui.same_line();
    ui.set_next_item_width(80.0);
    Drag::new("Batch Size")
        .speed(1.0)
        .range(1, 1000)
        .build(ui, &mut batch_size);

    ui.same_line();
    ui.checkbox("ID Prefix", &mut show_id_prefix);

    ui.same_line();
    if ui.button("Generate") {
        generate_initial_batches(link_type_index, start_id, batch_size, show_id_prefix);
    }

    {
        let mut cfg = RUNTIME_CONFIG.lock();
        cfg.link_type_index = link_type_index;
        cfg.start_id = start_id;
        cfg.batch_size = batch_size;
        cfg.show_id_prefix = show_id_prefix;
    }

    ui.separator();

    let generated = {
        let state = BATCH_STATE.lock();
        state.generated
    };

    if !generated {
        ui.text_disabled(
            "Press Generate to create batches. Scroll down for more, scroll up for previous.",
        );
        return;
    }

    if ui.button("< Previous Batch") {
        prepend_backward_batch(show_id_prefix);
    }
    ui.same_line();
    if ui.button("Next Batch >") {
        append_forward_batch(show_id_prefix);
    }

    let state = BATCH_STATE.lock();
    for batch in &state.batches {
        let color = BATCH_COLORS[batch.color_index % BATCH_COLORS.len()];
        let _color_token = ui.push_style_color(StyleColor::Text, color);
        ui.text(format!("--- IDs {} - {} ---", batch.start_id, batch.end_id));
        ui.same_line_with_pos(ui.content_region_avail()[0] - 30.0);
        let copy_label = format!("Copy##{}", batch.start_id);
        if ui.small_button(&copy_label) {
            ui.set_clipboard_text(&batch.text);
        }
        ui.text_wrapped(&batch.text);
        ui.spacing();
    }
}

fn render_individual_tab(ui: &Ui) {
    let mut state = INDIVIDUAL_STATE.lock();

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    if ui.small_button("Load All API Types##api_data") {
        db::ensure_all_loaded();
    }
    ui.same_line();
    if ui.small_button("Update All API Types##api_data") {
        db::update_all();
    }
    ui.separator();

    ui.set_next_item_width(180.0);
    let prev = state.prev_selected_type;
    ui.combo(
        "Type##individual",
        &mut state.selected_type,
        &type_names,
        |name| Cow::Borrowed(name),
    );

    let link_type = LinkType::ALL[state.selected_type];
    db::ensure_loaded(link_type);

    // Lazy load trigger on type change
    if state.selected_type != prev {
        state.prev_selected_type = state.selected_type;
        db::ensure_loaded(link_type);
    }

    // DB status bar for current type
    render_db_status(ui, link_type);

    ui.separator();

    match link_type {
        LinkType::Item => render_item_fields(ui, &mut state.item_fields),
        _ => {
            let idx = state.selected_type;
            let fields = &mut state.searchable[idx];
            render_searchable_fields(ui, link_type, fields);
        }
    }
}

fn render_ingame_item_db_tab(ui: &Ui) {
    db::ingame_items::ensure_loaded();
    let (selected_type, include_api_on_name_parse, include_descriptions_on_name_parse) = {
        let st = INGAME_DB_UI_STATE.lock();
        (
            LinkType::ALL[st.selected_type],
            st.include_api_on_name_parse,
            st.include_descriptions_on_name_parse,
        )
    };
    let is_item_type = selected_type == LinkType::Item;
    let db_label = format!("In-Game {} DB", selected_type.name());

    let (status, count, error, progress) = db::ingame_items::get_status();
    let count_display = if is_item_type {
        count
    } else {
        db::ingame_items::get_game_data_for_link_type(selected_type, "", usize::MAX).len()
    };
    match status {
        DbStatus::NotLoaded => ui.text_disabled(format!("{}: not built.", db_label)),
        DbStatus::Loading => {
            ui.text(format!("{} building... {} entries", db_label, format_number(count_display)));
            if let Some((done, total)) = progress {
                ui.text_disabled(format!(
                    "Progress: {} / {}",
                    format_number(done),
                    format_number(total)
                ));
            }
        }
        DbStatus::Loaded => {
            ui.text(format!("{}: {} entries", db_label, format_number(count_display)));
        }
        DbStatus::Updating => {
            ui.text(format!("{} updating... {} entries", db_label, format_number(count_display)));
            if let Some((done, total)) = progress {
                ui.text_disabled(format!(
                    "Progress: {} / {}",
                    format_number(done),
                    format_number(total)
                ));
            }
        }
        DbStatus::Error => {
            let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.3, 0.3, 1.0]);
            ui.text(format!("{} error: {}", db_label, error));
        }
    }

    // Fixed action layout for every type (same rows/order).
    if ui.small_button("Update##ingame_db_actions") {
        if is_item_type {
            if status == DbStatus::NotLoaded {
                db::ingame_items::rebuild();
            } else {
                db::ingame_items::update();
            }
        } else {
            db::ingame_items::start_build_game_data_for_link_type(selected_type, false);
        }
        return;
    }
    ui.same_line();
    if ui.small_button("Rebuild##ingame_db_actions") {
        if is_item_type {
            db::ingame_items::rebuild();
        } else {
            db::ingame_items::start_build_game_data_for_link_type(selected_type, true);
        }
        return;
    }

    if ui.small_button("Parse Names From Hashes##ingame_db_actions") {
        if is_item_type {
            db::ingame_items::parse_names_from_hashes(
                include_api_on_name_parse,
                include_descriptions_on_name_parse,
            );
        } else {
            db::ingame_items::parse_game_type_names_from_hashes(selected_type);
        }
        return;
    }
    ui.same_line();
    if ui.small_button("Full Rebuild Names##ingame_db_actions") {
        if is_item_type {
            db::ingame_items::full_rebuild_names_from_hashes(
                include_api_on_name_parse,
                include_descriptions_on_name_parse,
            );
        } else {
            db::ingame_items::full_rebuild_game_type_names_from_hashes(selected_type);
        }
        return;
    }
    if selected_type == LinkType::Map {
        if ui.small_button("Parse Map Names##ingame_db_actions") {
            db::ingame_items::parse_map_names_from_hashes();
            return;
        }
        ui.same_line();
        if ui.small_button("Full Rebuild Map Names##ingame_db_actions") {
            db::ingame_items::full_rebuild_map_names_from_hashes();
            return;
        }
    }

    if is_item_type {
        if db::ingame_items::is_name_parse_paused() {
            if ui.small_button("Resume Parse##ingame_db_actions") {
                db::ingame_items::set_name_parse_paused(false);
                return;
            }
        } else if status == DbStatus::Updating && ui.small_button("Pause Parse##ingame_db_actions")
        {
            db::ingame_items::set_name_parse_paused(true);
            return;
        }
    } else if db::ingame_items::is_build_missing_game_data_paused() {
        if ui.small_button("Resume Parse##ingame_db_actions") {
            db::ingame_items::set_build_missing_game_data_paused(false);
            return;
        }
    } else if db::ingame_items::get_build_missing_game_data_progress().is_some()
        && ui.small_button("Pause Parse##ingame_db_actions")
    {
        db::ingame_items::set_build_missing_game_data_paused(true);
        return;
    }

    {
        let mut st = INGAME_DB_UI_STATE.lock();
        let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();
        ui.set_next_item_width(240.0);
        ui.combo(
            "Type##ingame_db",
            &mut st.selected_type,
            &type_names,
            |name| Cow::Borrowed(name),
        );
        let selected_type = LinkType::ALL[st.selected_type];
        if selected_type == LinkType::Item {
            ui.checkbox(
                "Include API-defined names in name parse##ingame_db",
                &mut st.include_api_on_name_parse,
            );
            ui.checkbox(
                "Parse item descriptions too##ingame_db",
                &mut st.include_descriptions_on_name_parse,
            );
        }
    }

    ui.separator();

    let mut st = INGAME_DB_UI_STATE.lock();
    let selected_type = LinkType::ALL[st.selected_type];
    ui.set_next_item_width(260.0);
    let mut search_buf = st.search.clone();
    let submitted = InputText::new(ui, "Search##ingame_db", &mut search_buf)
        .hint("Name or ID...")
        .flags(InputTextFlags::ENTER_RETURNS_TRUE)
        .build();
    st.search = search_buf;
    ui.same_line();
    let api_filter_names = ["All", "Present on API", "Not on API"];
    ui.set_next_item_width(150.0);
    ui.combo(
        "API Filter##ingame_db",
        &mut st.api_filter,
        &api_filter_names,
        |name| Cow::Borrowed(name),
    );

    if selected_type == LinkType::Wardrobe {
        ui.same_line();
        let flag_filter_names = [
            "Flags: All",
            "Has ShowInWardrobe",
            "Not ShowInWardrobe",
            "Has HideIfLocked",
            "Not HideIfLocked",
            "Has NoCost",
            "Not NoCost",
            "Has OverrideRarity",
            "No OverrideRarity",
        ];
        ui.set_next_item_width(180.0);
        ui.combo(
            "Wardrobe Flags##ingame_db",
            &mut st.wardrobe_flag_filter,
            &flag_filter_names,
            |name| Cow::Borrowed(name),
        );
    } else if selected_type == LinkType::Map {
        ui.same_line();
        let poi_filter_names = [
            "PoI Type: All",
            "PointOfInterest",
            "Waypoint",
            "Unlock",
            "Vista",
            "Unknown/Other",
        ];
        ui.set_next_item_width(170.0);
        ui.combo(
            "PoI Type##ingame_db",
            &mut st.map_poi_filter,
            &poi_filter_names,
            |name| Cow::Borrowed(name),
        );
    }

    let sort_names = [
        "Default",
        "ID Asc",
        "ID Desc",
        "Name A-Z",
        "Name Z-A",
        "Name Length Asc",
        "Name Length Desc",
    ];
    ui.set_next_item_width(140.0);
    ui.combo("Sort##ingame_db", &mut st.sort_mode, &sort_names, |name| {
        Cow::Borrowed(name)
    });
    if selected_type == LinkType::Item {
        ui.set_next_item_width(120.0);
        Drag::new("Min ID##ingame_db")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut st.min_id);
        ui.same_line();
        ui.set_next_item_width(120.0);
        Drag::new("Max ID##ingame_db")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut st.max_id);
        ui.same_line();
    }
    ui.checkbox("Show chars##ingame_db", &mut st.show_char_count);

    let changed_filter = st.selected_type != st.last_selected_type
        || st.api_filter != st.last_api_filter
        || st.search != st.last_query
        || st.min_id != st.last_min_id
        || st.max_id != st.last_max_id
        || st.sort_mode != st.last_sort_mode
        || st.wardrobe_flag_filter != st.last_wardrobe_flag_filter
        || st.map_poi_filter != st.last_map_poi_filter;
    if submitted || changed_filter {
        let mut rows = if selected_type == LinkType::Item {
            db::ingame_items::search(&st.search, false, usize::MAX)
        } else {
            db::ingame_items::get_game_data_for_link_type(selected_type, &st.search, usize::MAX)
        };
        if selected_type == LinkType::Item {
            let min_id = st.min_id.max(0) as u32;
            let max_id = if st.max_id > 0 {
                Some(st.max_id as u32)
            } else {
                None
            };
            rows.retain(|r| {
                if r.id < min_id {
                    return false;
                }
                if let Some(max_i) = max_id {
                    if r.id > max_i {
                        return false;
                    }
                }
                true
            });
        } else if selected_type == LinkType::Wardrobe && st.wardrobe_flag_filter != 0 {
            let flag_filter = st.wardrobe_flag_filter;
            rows.retain(|r| {
                let Some(extra) =
                    db::ingame_items::get_game_data_entry_for_link_type(selected_type, r.id)
                else {
                    return false;
                };
                wardrobe_matches_flag_filter(extra.skin_flags_code, flag_filter)
            });
        } else if selected_type == LinkType::Map && st.map_poi_filter != 0 {
            let poi_filter = st.map_poi_filter;
            rows.retain(|r| {
                let Some(extra) =
                    db::ingame_items::get_game_data_entry_for_link_type(selected_type, r.id)
                else {
                    return false;
                };
                map_matches_poi_filter(extra.poi_type_code, poi_filter)
            });
        }
        rows.retain(|r| {
            if st.api_filter == 1 && !r.in_api {
                return false;
            }
            if st.api_filter == 2 && r.in_api {
                return false;
            }
            true
        });
        match st.sort_mode {
            1 => rows.sort_by_key(|r| r.id),
            2 => rows.sort_by(|a, b| b.id.cmp(&a.id)),
            3 => rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            4 => rows.sort_by(|a, b| b.name.to_lowercase().cmp(&a.name.to_lowercase())),
            5 => rows.sort_by_key(|r| r.name.chars().count()),
            6 => rows.sort_by(|a, b| b.name.chars().count().cmp(&a.name.chars().count())),
            _ => {}
        }
        st.results = rows;
        st.page = 0;
        st.last_selected_type = st.selected_type;
        st.last_query = st.search.clone();
        st.last_api_filter = st.api_filter;
        st.last_min_id = st.min_id;
        st.last_max_id = st.max_id;
        st.last_sort_mode = st.sort_mode;
        st.last_wardrobe_flag_filter = st.wardrobe_flag_filter;
        st.last_map_poi_filter = st.map_poi_filter;
    }

    let show_char_count = st.show_char_count;
    let (new_page, selected_id) = render_ingame_table_rows(
        ui,
        "ingame_db_results_table",
        &st.results,
        selected_type,
        st.page,
        show_char_count,
    );
    st.page = new_page;
    if let Some(id) = selected_id {
        st.selected_id = id as i32;
    }

    ui.set_next_item_width(140.0);
    Drag::new("Selected ID##ingame")
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut st.selected_id);

    if st.selected_id > 0 {
        let selected_id_u32 = st.selected_id as u32;
        if selected_type == LinkType::Item {
            if let Some(item) = db::ingame_items::get_item(selected_id_u32) {
                ui.separator();
                ui.text(format!("Name: {}", item.name));
                ui.text(format!("ID: {}", item.id));
                ui.text(format!("Chat Link: {}", item.chat_link));
                if ui.small_button("Copy Chat Code##ingame") {
                    ui.set_clipboard_text(&item.chat_link);
                }
                ui.text(format!(
                    "In API: {}",
                    if item.in_api { "Yes" } else { "No" }
                ));
                ui.text(format!(
                    "Type/Rarity: {} ({}) / {} ({})",
                    ingame_item_type_label(item.item_type_code),
                    item.item_type_code,
                    ingame_rarity_label(item.rarity_code),
                    item.rarity_code
                ));
                ui.text(format!(
                    "Required Level: {} | Vendor Value: {} | Gemstore: {}",
                    item.required_level,
                    item.vendor_value,
                    if item.is_gemstore { "Yes" } else { "No" }
                ));
                ui.text(format!(
                    "Default Skin ID: {}",
                    if item.default_skin_id == 0 {
                        "<none>".to_string()
                    } else {
                        item.default_skin_id.to_string()
                    }
                ));
                if !item.upgrade_name.trim().is_empty() {
                    ui.text(format!("Upgrade Name: {}", item.upgrade_name));
                }
                if !item.description.is_empty() {
                    ui.text_wrapped(&item.description);
                }
            }
        } else if let Some(row) = st.results.iter().find(|r| r.id == selected_id_u32) {
            ui.separator();
            ui.text(format!("Name: {}", row.name));
            ui.text(format!("ID: {}", row.id));
            let code = encoder::generate_batch_link(selected_type, row.id);
            ui.text(format!("Chat Link: {}", code));
            if ui.small_button("Copy Chat Code##ingame_non_item") {
                ui.set_clipboard_text(&code);
            }
            ui.text(format!("In API: {}", if row.in_api { "Yes" } else { "No" }));
            if let Some(extra) =
                db::ingame_items::get_game_data_entry_for_link_type(selected_type, row.id)
            {
                ui.text(format!(
                    "Name Hash: {} | Description Hash: {}",
                    extra.name_hash, extra.description_hash
                ));
                if selected_type == LinkType::Wardrobe {
                    ui.text(format!(
                        "Skin Type/Rarity: {} ({}) / {} ({})",
                        ingame_item_type_label(extra.skin_type_code),
                        extra.skin_type_code,
                        ingame_rarity_label(extra.skin_rarity_code),
                        extra.skin_rarity_code
                    ));
                    ui.text(format!(
                        "Skin Flags (0x50): {} ({})",
                        extra.skin_flags_code,
                        guess_skin_flags_0x50(extra.skin_flags_code)
                    ));
                }
                if selected_type == LinkType::Map {
                    ui.text(format!(
                        "PoI Type: {} ({})",
                        poi_type_label(extra.poi_type_code),
                        extra.poi_type_code
                    ));
                    ui.text(format!(
                        "Map: {}",
                        if extra.map_name.is_empty() {
                            "<unresolved>"
                        } else {
                            &extra.map_name
                        }
                    ));
                    ui.text(format!("Map Name Hash: {}", extra.map_name_hash));
                }
                if !extra.description.is_empty() {
                    ui.text_wrapped(&extra.description);
                }
            }
        }
    }

}

fn render_all_data_tab(ui: &Ui) {
    let mut st = ALL_DATA_UI_STATE.lock();
    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    ui.set_next_item_width(240.0);
    ui.combo(
        "Content Type##all_data",
        &mut st.selected_type,
        &type_names,
        |name| Cow::Borrowed(name),
    );
    let link_type = LinkType::ALL[st.selected_type];
    db::ensure_loaded(link_type);
    let (api_db_status, api_db_count, _, _) = db::get_status(link_type);

    ui.separator();
    let force_refresh = ui.small_button("Refresh##all_data");

    ui.set_next_item_width(130.0);
    let row_mode_names = ["Merged", "Non-Merged"];
    ui.combo(
        "Rows##all_data",
        &mut st.row_mode,
        &row_mode_names,
        |name| Cow::Borrowed(name),
    );
    ui.same_line();
    ui.set_next_item_width(260.0);
    let mut search_buf = st.search.clone();
    let _submitted = InputText::new(ui, "Search##all_data", &mut search_buf)
        .hint("Name or ID...")
        .flags(InputTextFlags::ENTER_RETURNS_TRUE)
        .build();
    st.search = search_buf;
    let cache_stale = st.cache_type != st.selected_type
        || st.cache_search != st.search
        || st.cache_row_mode != st.row_mode
        || st.cache_api_db_count != api_db_count
        || st.cache_api_db_status != api_db_status;
    if cache_stale || force_refresh {
        let api_rows = db::search(link_type, &st.search, 0, usize::MAX);
        let game_rows =
            db::ingame_items::get_game_data_for_link_type(link_type, &st.search, usize::MAX);

        st.cache_rows = if st.row_mode == 0 {
            let mut merged: BTreeMap<u32, (Option<String>, Option<String>)> = BTreeMap::new();
            for (id, name) in &api_rows {
                merged.entry(*id).or_default().0 = Some(name.clone());
            }
            for row in &game_rows {
                merged.entry(row.id).or_default().1 = Some(row.name.clone());
            }
            merged
                .into_iter()
                .map(|(id, (api_raw, game_raw))| {
                    let api_norm = normalize_all_data_name(api_raw.as_deref().unwrap_or("-"));
                    let game_norm = normalize_all_data_name(game_raw.as_deref().unwrap_or("-"));
                    let matched = !api_norm.is_empty()
                        && !game_norm.is_empty()
                        && api_norm.eq_ignore_ascii_case(&game_norm);
                    AllDataRow {
                        id,
                        api_raw,
                        game_raw,
                        api_norm,
                        game_norm,
                        matched,
                    }
                })
                .collect()
        } else {
            let mut out = Vec::with_capacity(api_rows.len().saturating_add(game_rows.len()));
            for (id, name) in &api_rows {
                let api_norm = normalize_all_data_name(name);
                out.push(AllDataRow {
                    id: *id,
                    api_raw: Some(name.clone()),
                    game_raw: None,
                    api_norm,
                    game_norm: String::new(),
                    matched: false,
                });
            }
            for row in &game_rows {
                let game_norm = normalize_all_data_name(&row.name);
                out.push(AllDataRow {
                    id: row.id,
                    api_raw: None,
                    game_raw: Some(row.name.clone()),
                    api_norm: String::new(),
                    game_norm,
                    matched: false,
                });
            }
            out.sort_by_key(|r| r.id);
            out
        };
        st.cache_api_count = api_rows.len();
        st.cache_game_count = game_rows.len();
        st.cache_api_db_count = api_db_count;
        st.cache_api_db_status = api_db_status;
        st.cache_type = st.selected_type;
        st.cache_search = st.search.clone();
        st.cache_row_mode = st.row_mode;
        st.page = 0;
        st.jump_page = 1;
    }

    ui.separator();

    let total_pages = st.cache_rows.len().div_ceil(RESULTS_PER_PAGE).max(1);
    if st.page >= total_pages {
        st.page = total_pages - 1;
    }
    let start = st.page * RESULTS_PER_PAGE;
    let end = (start + RESULTS_PER_PAGE).min(st.cache_rows.len());

    ui.text(format!(
        "Merged Rows: {} | API: {} | Game: {}",
        st.cache_rows.len(),
        st.cache_api_count,
        st.cache_game_count
    ));
    ui.same_line();
    ui.text_disabled(format!("Page {} / {}", st.page + 1, total_pages));
    ui.same_line();
    if ui.small_button("First##all_data_page") {
        st.page = 0;
    }
    ui.same_line();
    if ui.small_button("Prev##all_data_page") && st.page > 0 {
        st.page -= 1;
    }
    ui.same_line();
    if ui.small_button("Next##all_data_page") && st.page + 1 < total_pages {
        st.page += 1;
    }
    ui.same_line();
    if ui.small_button("Last##all_data_page") {
        st.page = total_pages.saturating_sub(1);
    }
    ui.same_line();
    ui.set_next_item_width(80.0);
    let _ = Drag::new("Page##all_data_jump")
        .speed(1.0)
        .range(1, total_pages as i32)
        .build(ui, &mut st.jump_page);
    ui.same_line();
    if ui.small_button("Go##all_data_page") {
        let p = st.jump_page.clamp(1, total_pages as i32) as usize;
        st.page = p - 1;
    }

    if let Some(_t) = ui.begin_table_with_flags(
        "##all_data_merged_table",
        4,
        TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE,
    ) {
        ui.table_setup_column("ID");
        ui.table_setup_column("API Name");
        ui.table_setup_column("Game Name");
        ui.table_setup_column("Match");
        ui.table_headers_row();

        let mut clicked_id: Option<u32> = None;
        for row in &st.cache_rows[start..end] {
            let match_label = if row.matched { "Same" } else { "Diff/Missing" };
            let row_color = if row.matched {
                [0.45, 0.90, 0.55, 1.0] // green
            } else if row.api_raw.is_some() && row.game_raw.is_some() {
                [1.00, 0.55, 0.55, 1.0] // red (both present but different)
            } else {
                [0.95, 0.85, 0.45, 1.0] // yellow (missing one side)
            };

            ui.table_next_row();
            let _row_tint = ui.push_style_color(StyleColor::Text, row_color);
            ui.table_next_column();
            let selected = st.selected_id == row.id;
            let clicked = Selectable::new(&format!("{}##all_data_id_{}", row.id, row.id))
                .selected(selected)
                .span_all_columns(true)
                .allow_double_click(false)
                .build(ui);
            if clicked {
                clicked_id = Some(row.id);
            }
            ui.table_next_column();
            ui.text(if row.api_norm.is_empty() {
                "-"
            } else {
                &row.api_norm
            });
            ui.table_next_column();
            ui.text(if row.game_norm.is_empty() {
                "-"
            } else {
                &row.game_norm
            });
            ui.table_next_column();
            ui.text(match_label);
        }
        if let Some(id) = clicked_id {
            st.selected_id = id;
        }
    }

    if st.selected_id != 0 {
        let selected = st.selected_id;
        let selected_row = st.cache_rows.iter().find(|r| r.id == selected).cloned();
        let (api_raw, game_raw, api_norm, game_norm) = match selected_row {
            Some(r) => (r.api_raw, r.game_raw, r.api_norm, r.game_norm),
            None => (None, None, String::new(), String::new()),
        };

        ui.separator();
        let api_raw_text = api_raw.as_deref().unwrap_or("-");
        let game_raw_text = game_raw.as_deref().unwrap_or("-");
        ui.text(format!("Selected ID: {}", selected));
        ui.text(format!(
            "API Name: {}",
            if api_norm.is_empty() { "-" } else { &api_norm }
        ));
        ui.text(format!(
            "Game Name: {}",
            if game_norm.is_empty() {
                "-"
            } else {
                &game_norm
            }
        ));
        ui.text_disabled(format!("Raw API: {}", api_raw_text));
        ui.text_disabled(format!("Raw Game: {}", game_raw_text));
        let api_url = api_url_for_type(link_type, selected);
        ui.text(format!("API URL: {}", api_url));
        if ui.small_button("Copy API URL##all_data") {
            ui.set_clipboard_text(&api_url);
        }
    }
}

fn render_debug_probe_tab(ui: &Ui) {
    let mut st = DEBUG_PROBE_UI_STATE.lock();
    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    ui.set_next_item_width(240.0);
    ui.combo(
        "Type##debug_probe_type",
        &mut st.selected_type,
        &type_names,
        |name| Cow::Borrowed(name),
    );
    ui.same_line();
    ui.set_next_item_width(140.0);
    Drag::new("ID##debug_probe_id")
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut st.id);
    ui.same_line();
    ui.set_next_item_width(120.0);
    Drag::new("Offset##debug_probe_offset")
        .speed(1.0)
        .range(0, 0x200)
        .build(ui, &mut st.selected_offset);
    ui.same_line();
    ui.checkbox("Custom Content Type##debug_probe_custom_toggle", &mut st.use_custom_content_type);
    if st.use_custom_content_type {
        ui.same_line();
        ui.set_next_item_width(140.0);
        Drag::new("ContentType##debug_probe_custom_type")
            .speed(1.0)
            .range(0, 10000)
            .build(ui, &mut st.custom_content_type);
    }

    let id = st.id.max(0) as u32;
    let link_type = LinkType::ALL[st.selected_type];
    let content_type_override = if st.use_custom_content_type {
        Some(st.custom_content_type.max(0) as u32)
    } else {
        None
    };

    if st.resolve_pending {
        if let Some(res) = db::ingame_items::consume_debug_resolve_result() {
            st.resolve_pending = false;
            match res {
                Ok(v) => {
                    st.last_resolve = Some(v);
                    st.last_resolve_error.clear();
                }
                Err(err) => {
                    st.last_resolve = None;
                    st.last_resolve_error = err;
                }
            }
        }
    }

    if ui.small_button("Probe Content Offsets##debug_probe_run") {
        let link_type = LinkType::ALL[st.selected_type];
        let id = st.id.max(0) as u32;
        match db::ingame_items::debug_probe_content_for_content_type(
            link_type,
            id,
            content_type_override,
        ) {
            Ok(info) => {
                st.last_info = Some(info);
                st.last_error.clear();
                st.subdef_mode = false;
                st.resolve_from_subdef = false;
            }
            Err(err) => {
                st.last_info = None;
                st.last_error = err;
            }
        }
    }
    if link_type == LinkType::Item {
        ui.same_line();
        if ui.small_button("Probe Item Subdef##debug_probe_run_subdef") {
            match db::ingame_items::debug_probe_item_subdef_for_content_type(
                link_type,
                id,
                content_type_override,
            ) {
                Ok(info) => {
                    st.last_info = Some(info);
                    st.last_error.clear();
                    st.subdef_mode = true;
                    st.resolve_from_subdef = true;
                    if st.selected_subdef_offset < 0 {
                        st.selected_subdef_offset = 0x60;
                    }
                }
                Err(err) => {
                    st.last_info = None;
                    st.last_error = err;
                }
            }
        }
    }
    ui.same_line();
    if ui.small_button("Resolve Selected Offset##debug_probe_resolve") {
        let (off, raw) = if st.resolve_from_subdef {
            let off = st.selected_subdef_offset.max(0) as usize;
            let raw = st
                .last_info
                .as_ref()
                .and_then(|i| i.subdef_rows.iter().find(|r| r.offset == off))
                .map(|r| r.raw_u32)
                .unwrap_or(0);
            (off, raw)
        } else {
            let off = st.selected_offset.max(0) as usize;
            let raw = st
                .last_info
                .as_ref()
                .and_then(|i| i.rows.iter().find(|r| r.offset == off))
                .map(|r| r.raw_u32)
                .unwrap_or(0);
            (off, raw)
        };
        if raw == 0 {
            st.last_resolve_error = "Selected offset has value 0".to_string();
            st.last_resolve = None;
        } else {
            let res = if st.resolve_from_subdef {
                let source_ptr = st
                    .last_info
                    .as_ref()
                    .map(|i| i.subdef_ptr)
                    .filter(|p| *p != 0);
                db::ingame_items::queue_debug_resolve_hash(link_type, id, off, raw, source_ptr)
            } else {
                db::ingame_items::queue_debug_resolve_offset_for_content_type(
                    link_type,
                    id,
                    content_type_override,
                    off,
                )
            };
            match res {
                Ok(_) => {
                    st.resolve_pending = true;
                    st.last_resolve = None;
                    st.last_resolve_error.clear();
                }
                Err(err) => {
                    st.resolve_pending = false;
                    st.last_resolve = None;
                    st.last_resolve_error = err;
                }
            }
        }
    }

    if !st.last_error.is_empty() {
        let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.3, 0.3, 1.0]);
        ui.text(&st.last_error);
    }

    let info = st.last_info.clone();
    let subdef_mode = st.subdef_mode;
    let resolve_pending = st.resolve_pending;
    let last_resolve = st.last_resolve.clone();
    let last_resolve_error = st.last_resolve_error.clone();
    drop(st);

    ui.separator();
    if resolve_pending {
        ui.text_disabled("Resolve queue: pending...");
    } else {
        ui.text_disabled("Resolve queue: idle");
    }
    if !last_resolve_error.is_empty() {
        let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.3, 0.3, 1.0]);
        ui.text(format!("Resolve error: {}", last_resolve_error));
    } else if let Some(r) = last_resolve {
        ui.text(format!(
            "Resolved hash at 0x{:X}: {} (0x{:08X})",
            r.offset, r.raw_u32, r.raw_u32
        ));
        if let Some(decoded) = r.decoded_text {
            ui.text_wrapped(&decoded);
        } else {
                ui.text_disabled("Decoded text unavailable; showing coded text:");
                ui.text_wrapped(&r.coded_text);
        }
    }
    ui.text_disabled("Raw probe with strict type+id lookup. Resolve is allowed for any non-zero value.");

    let Some(info) = info else {
        ui.separator();
        ui.text_disabled("Probe a type + id to inspect raw offsets.");
        return;
    };

    ui.separator();
    ui.text(format!(
        "Type: {} | ID: {} | ContentType: {} | ResolvedType: {}",
        info.link_type.name(),
        info.id,
        info.content_type,
        info.resolved_content_type
    ));
    ui.text(format!(
        "Ptr: 0x{:X} | DefType: {} | DefIndex: {}",
        info.content_ptr, info.content_def_type, info.content_def_index
    ));
    match info.known_name_offset {
        Some(off) => ui.text_disabled(format!("Known name offset for this type: 0x{:X}", off)),
        None => ui.text_disabled("Known name offset for this type: <unknown>"),
    }
    match info.discovered_name_offset {
        Some(off) => ui.text_disabled(format!(
            "Discovered name offset from previous scans: 0x{:X}",
            off
        )),
        None => ui.text_disabled("Discovered name offset from previous scans: <none>"),
    }
    match info.discovered_id_offset {
        Some(off) => ui.text_disabled(format!(
            "Discovered id offset from previous scans: 0x{:X}",
            off
        )),
        None => ui.text_disabled("Discovered id offset from previous scans: <none>"),
    }

    ui.text_disabled("Offset table is raw only. No text resolve/decode is performed.");
    if ui.small_button("Copy Full Dump##debug_probe_copy_dump") {
        let mut dump = String::new();
        dump.push_str(&format!(
            "Type={} ID={} ContentType={} ResolvedType={} Ptr=0x{:X} DefType={} DefIndex={}\n",
            info.link_type.name(),
            info.id,
            info.content_type,
            info.resolved_content_type,
            info.content_ptr,
            info.content_def_type,
            info.content_def_index
        ));
        if let Some(off) = info.known_name_offset {
            dump.push_str(&format!("KnownNameOffset=0x{:X}\n", off));
        } else {
            dump.push_str("KnownNameOffset=<unknown>\n");
        }
        if let Some(off) = info.discovered_name_offset {
            dump.push_str(&format!("DiscoveredNameOffset=0x{:X}\n", off));
        } else {
            dump.push_str("DiscoveredNameOffset=<none>\n");
        }
        if let Some(off) = info.discovered_id_offset {
            dump.push_str(&format!("DiscoveredIdOffset=0x{:X}\n", off));
        } else {
            dump.push_str("DiscoveredIdOffset=<none>\n");
        }
        dump.push_str("offset,dec,hex,bits,possible_hash\n");
        for row in &info.rows {
            dump.push_str(&format!(
                "0x{:X},{},0x{:08X},\"{}\",{}\n",
                row.offset,
                row.raw_u32,
                row.raw_u32,
                format_set_bits(row.raw_u32),
                if row.is_hash_candidate { "yes" } else { "no" }
            ));
        }
        if info.subdef_ptr != 0 {
            dump.push_str(&format!("SubdefPtr=0x{:X}\n", info.subdef_ptr));
            dump.push_str("sub_offset,dec,hex,bits,possible_hash\n");
            for row in &info.subdef_rows {
                dump.push_str(&format!(
                    "0x{:X},{},0x{:08X},\"{}\",{}\n",
                    row.offset,
                    row.raw_u32,
                    row.raw_u32,
                    format_set_bits(row.raw_u32),
                    if row.is_hash_candidate { "yes" } else { "no" }
                ));
            }
        }
        ui.set_clipboard_text(&dump);
    }
    ui.same_line();
    ui.text_disabled(format!(
        "Rows: {}",
        if subdef_mode {
            info.subdef_rows.len()
        } else {
            info.rows.len()
        }
    ));

    let mut st_for_filter = DEBUG_PROBE_UI_STATE.lock();
    ui.set_next_item_width(180.0);
    Drag::new("Field Value##debug_probe_field_value")
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut st_for_filter.field_search_value);
    ui.same_line();
    ui.checkbox(
        "Only Matching Rows##debug_probe_only_matching",
        &mut st_for_filter.only_matching_fields,
    );
    let field_search_value = st_for_filter.field_search_value.max(0) as u32;
    let only_matching_fields = st_for_filter.only_matching_fields;
    let selected_offset = st_for_filter.selected_offset.max(0) as usize;
    let selected_subdef_offset = st_for_filter.selected_subdef_offset.max(0) as usize;
    let resolve_from_subdef = st_for_filter.resolve_from_subdef;
    drop(st_for_filter);

    if !resolve_from_subdef || !subdef_mode {
        if let Some(selected_row) = info.rows.iter().find(|r| r.offset == selected_offset) {
            ui.text_disabled(format!(
                "Selected offset 0x{:X}: dec={} hex=0x{:08X} bits=[{}]",
                selected_offset,
                selected_row.raw_u32,
                selected_row.raw_u32,
                format_set_bits(selected_row.raw_u32)
            ));
            if info.link_type == LinkType::Wardrobe && selected_offset == 0x50 {
                ui.text_disabled(format!(
                    "Skin 0x50 guessed flags: {}",
                    guess_skin_flags_0x50(selected_row.raw_u32)
                ));
            }
        }
    } else if let Some(selected_row) = info
        .subdef_rows
        .iter()
        .find(|r| r.offset == selected_subdef_offset)
    {
        ui.text_disabled(format!(
            "Selected subdef offset 0x{:X}: dec={} hex=0x{:08X} bits=[{}]",
            selected_subdef_offset,
            selected_row.raw_u32,
            selected_row.raw_u32,
            format_set_bits(selected_row.raw_u32)
        ));
    }

    if !subdef_mode {
        if let Some(_t) = ui.begin_table_with_flags(
            "##debug_probe_offsets_table",
            5,
            TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE,
        ) {
            ui.table_setup_column("Offset");
            ui.table_setup_column("Raw (dec)");
            ui.table_setup_column("Raw (hex)");
            ui.table_setup_column("Bits");
            ui.table_setup_column("Possible Hash");
            ui.table_headers_row();

            let mut clicked_offset: Option<usize> = None;
            for row in &info.rows {
                let field_match = field_search_value > 0 && row.raw_u32 == field_search_value;
                if only_matching_fields && !field_match {
                    continue;
                }
                ui.table_next_row();
                ui.table_next_column();
                let label = format!("0x{:X}##debug_probe_off_{:X}", row.offset, row.offset);
                let clicked = Selectable::new(&label)
                    .span_all_columns(true)
                    .build(ui);
                if clicked {
                    clicked_offset = Some(row.offset);
                }
                ui.table_next_column();
                if field_match {
                    ui.text_colored([0.95, 0.85, 0.35, 1.0], row.raw_u32.to_string());
                } else {
                    ui.text(row.raw_u32.to_string());
                }
                ui.table_next_column();
                if field_match {
                    ui.text_colored([0.95, 0.85, 0.35, 1.0], format!("0x{:08X}", row.raw_u32));
                } else {
                    ui.text(format!("0x{:08X}", row.raw_u32));
                }
                ui.table_next_column();
                ui.text(format_set_bits(row.raw_u32));
                ui.table_next_column();
                if row.is_hash_candidate {
                    ui.text_colored([0.45, 0.90, 0.55, 1.0], "Yes");
                } else {
                    ui.text_disabled("-");
                }
            }
            if let Some(off) = clicked_offset {
                let mut st = DEBUG_PROBE_UI_STATE.lock();
                st.selected_offset = off as i32;
                st.resolve_from_subdef = false;
            }
        }
    }

    if info.subdef_ptr != 0 {
        ui.separator();
        ui.text(format!("Subdef Ptr: 0x{:X}", info.subdef_ptr));
        if let Some(_t) = ui.begin_table_with_flags(
            "##debug_probe_subdef_offsets_table",
            5,
            TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE,
        ) {
            ui.table_setup_column("Subdef+Off");
            ui.table_setup_column("Raw (dec)");
            ui.table_setup_column("Raw (hex)");
            ui.table_setup_column("Bits");
            ui.table_setup_column("Possible Hash");
            ui.table_headers_row();

            let mut clicked_subdef_offset: Option<usize> = None;
            for row in &info.subdef_rows {
                let field_match = field_search_value > 0 && row.raw_u32 == field_search_value;
                if only_matching_fields && !field_match {
                    continue;
                }
                ui.table_next_row();
                ui.table_next_column();
                let label = format!("0x{:X}##debug_probe_subdef_off_{:X}", row.offset, row.offset);
                let clicked = Selectable::new(&label)
                    .span_all_columns(true)
                    .build(ui);
                if clicked {
                    clicked_subdef_offset = Some(row.offset);
                }
                ui.table_next_column();
                if field_match {
                    ui.text_colored([0.95, 0.85, 0.35, 1.0], row.raw_u32.to_string());
                } else {
                    ui.text(row.raw_u32.to_string());
                }
                ui.table_next_column();
                if field_match {
                    ui.text_colored([0.95, 0.85, 0.35, 1.0], format!("0x{:08X}", row.raw_u32));
                } else {
                    ui.text(format!("0x{:08X}", row.raw_u32));
                }
                ui.table_next_column();
                ui.text(format_set_bits(row.raw_u32));
                ui.table_next_column();
                if row.is_hash_candidate {
                    ui.text_colored([0.45, 0.90, 0.55, 1.0], "Yes");
                } else {
                    ui.text_disabled("-");
                }
            }
            if let Some(off) = clicked_subdef_offset {
                let mut st = DEBUG_PROBE_UI_STATE.lock();
                st.selected_subdef_offset = off as i32;
                st.resolve_from_subdef = true;
            }
        }
    }

}

fn api_url_for_type(link_type: LinkType, id: u32) -> String {
    match link_type {
        LinkType::Item => format!("https://api.guildwars2.com/v2/items/{}", id),
        LinkType::Skill => format!("https://api.guildwars2.com/v2/skills/{}", id),
        LinkType::Trait => format!("https://api.guildwars2.com/v2/traits/{}", id),
        LinkType::Recipe => format!("https://api.guildwars2.com/v2/recipes/{}", id),
        LinkType::Wardrobe => format!("https://api.guildwars2.com/v2/skins/{}", id),
        LinkType::Outfit => format!("https://api.guildwars2.com/v2/outfits/{}", id),
        LinkType::Map => format!(
            "https://api.guildwars2.com/v2/continents?ids=all (POI id: {})",
            id
        ),
    }
}

fn format_set_bits(value: u32) -> String {
    let mut bits = Vec::new();
    for bit in 0..32u32 {
        if ((value >> bit) & 1) == 1 {
            bits.push(bit.to_string());
        }
    }
    if bits.is_empty() {
        "-".to_string()
    } else {
        bits.join(",")
    }
}

fn guess_skin_flags_0x50(value: u32) -> String {
    let mut labels: Vec<String> = Vec::new();
    if (value & (1 << 0)) != 0 {
        labels.push("ShowInWardrobe".to_string());
    }
    if (value & (1 << 1)) != 0 {
        labels.push("HideIfLocked".to_string());
    }
    if (value & (1 << 3)) != 0 {
        labels.push("NoCost".to_string());
    }
    if (value & (1 << 7)) != 0 {
        labels.push("OverrideRarity".to_string());
    }
    let mut unknown_bits: Vec<String> = Vec::new();
    for bit in 0..32u32 {
        if ((value >> bit) & 1) == 1 && bit != 0 && bit != 1 && bit != 3 && bit != 7 {
            unknown_bits.push(bit.to_string());
        }
    }
    if !unknown_bits.is_empty() {
        labels.push(format!("UnknownBits:[{}]", unknown_bits.join(",")));
    }
    if labels.is_empty() {
        "none".to_string()
    } else {
        labels.join(", ")
    }
}

fn poi_type_label(value: u32) -> &'static str {
    match value {
        0 => "PointOfInterest",
        1 => "Waypoint",
        2 => "Unlock",
        3 => "Vista",
        _ => "Unknown",
    }
}

fn map_matches_poi_filter(poi_type: u32, filter_idx: usize) -> bool {
    match filter_idx {
        0 => true,
        1 => poi_type == 0,
        2 => poi_type == 1,
        3 => poi_type == 2,
        4 => poi_type == 3,
        5 => poi_type > 3,
        _ => true,
    }
}

fn wardrobe_matches_flag_filter(flags: u32, filter_idx: usize) -> bool {
    match filter_idx {
        0 => true,
        1 => (flags & (1 << 0)) != 0, // ShowInWardrobe
        2 => (flags & (1 << 0)) == 0, // Not ShowInWardrobe
        3 => (flags & (1 << 1)) != 0, // HideIfLocked
        4 => (flags & (1 << 1)) == 0, // Not HideIfLocked
        5 => (flags & (1 << 3)) != 0, // NoCost
        6 => (flags & (1 << 3)) == 0, // Not NoCost
        7 => (flags & (1 << 7)) != 0, // OverrideRarity
        8 => (flags & (1 << 7)) == 0, // No OverrideRarity
        _ => true,
    }
}

fn normalize_all_data_name(name: &str) -> String {
    let mut s = name.trim().to_string();
    if s.is_empty() || s == "-" {
        return String::new();
    }

    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            s = rest[end + 1..].trim_start().to_string();
        }
    }

    if let Some(rest) = s.strip_prefix('(') {
        if let Some(end) = rest.find(')') {
            let prefix = &rest[..end];
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
                s = rest[end + 1..].trim_start().to_string();
            }
        }
    }

    loop {
        let trimmed = s.trim_end();
        if !trimmed.ends_with(')') {
            break;
        }
        let Some(start) = trimmed.rfind(" (") else {
            break;
        };
        s = trimmed[..start].trim_end().to_string();
    }

    s
}

fn render_db_status(ui: &Ui, link_type: LinkType) {
    let (status, count, error_msg, progress) = db::get_status(link_type);

    match status {
        DbStatus::NotLoaded => {
            ui.text_disabled(&format!("{} DB: not built.", link_type.name()));
            ui.same_line();
            if ui.small_button("Build") {
                db::ensure_loaded(link_type);
            }
        }
        DbStatus::Loading => {
            if let Some((fetched, total)) = progress {
                let label = format!(
                    "Building {} DB... {} / {}",
                    link_type.name(),
                    format_number(fetched),
                    format_number(total),
                );
                ui.text_disabled(&label);

                if total > 0 {
                    let frac = fetched as f32 / total as f32;
                    nexus::imgui::ProgressBar::new(frac)
                        .size([200.0, 0.0])
                        .build(ui);
                }
            } else {
                ui.text_disabled(&format!("Loading {} DB...", link_type.name()));
            }
        }
        DbStatus::Loaded => {
            ui.text(format!(
                "{} DB: {} entries",
                link_type.name(),
                format_number(count),
            ));
            ui.same_line();
            if ui.small_button("Update") {
                db::update(link_type);
            }
            ui.same_line();
            if ui.small_button("Rebuild") {
                db::rebuild(link_type);
            }
        }
        DbStatus::Updating => {
            if let Some((fetched, total)) = progress {
                ui.text(format!(
                    "{} DB: {} entries — Updating... {} / {} new",
                    link_type.name(),
                    format_number(count),
                    format_number(fetched),
                    format_number(total),
                ));
            } else {
                ui.text(format!(
                    "{} DB: {} entries — Updating...",
                    link_type.name(),
                    format_number(count),
                ));
            }
        }
        DbStatus::Error => {
            let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.3, 0.3, 1.0]);
            ui.text(format!("{} DB failed: {}", link_type.name(), error_msg));
            ui.same_line();
            if ui.small_button("Retry") {
                db::ensure_loaded(link_type);
            }
        }
    }
}

fn format_number(n: usize) -> String {
    if n >= 1000 {
        let thousands = n / 1000;
        let remainder = n % 1000;
        format!("{},{:03}", thousands, remainder)
    } else {
        n.to_string()
    }
}

fn render_page_nav(ui: &Ui, label_suffix: &str, page: &mut usize, total: usize) {
    let total_pages = (total + RESULTS_PER_PAGE - 1) / RESULTS_PER_PAGE;
    if total_pages <= 1 {
        ui.text_disabled(format!("{} results", total));
        return;
    }

    if ui.small_button(&format!("|<< First##{}", label_suffix)) {
        *page = 0;
    }
    ui.same_line();
    if ui.small_button(&format!("< Prev##{}", label_suffix)) {
        *page = page.saturating_sub(1);
    }
    ui.same_line();
    ui.text(format!(
        "Page {} / {} ({} results)",
        *page + 1,
        total_pages,
        total
    ));
    ui.same_line();
    if ui.small_button(&format!("Next >##{}", label_suffix)) && *page + 1 < total_pages {
        *page += 1;
    }
    ui.same_line();
    if ui.small_button(&format!("Last >>|##{}", label_suffix)) {
        *page = total_pages - 1;
    }
}

fn render_item_table_rows(
    ui: &Ui,
    table_id: &str,
    rows: &[(u32, String)],
    mut page: usize,
    show_char_count: bool,
) -> (usize, Option<u32>) {
    if rows.is_empty() {
        ui.text_disabled("No results.");
        return (0, None);
    }

    let total = rows.len();
    let total_pages = (total + RESULTS_PER_PAGE - 1) / RESULTS_PER_PAGE;
    if page >= total_pages {
        page = total_pages.saturating_sub(1);
    }

    let start = page * RESULTS_PER_PAGE;
    let end = (start + RESULTS_PER_PAGE).min(total);
    let mut selected = None;

    let flags = TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE;
    let cols = if show_char_count { 3 } else { 2 };
    if let Some(_t) = ui.begin_table_with_flags(table_id, cols, flags) {
        ui.table_setup_column("ID");
        ui.table_setup_column("Name");
        if show_char_count {
            ui.table_setup_column("Chars");
        }
        ui.table_headers_row();

        for (id, name) in &rows[start..end] {
            ui.table_next_row();
            ui.table_next_column();
            let clicked = Selectable::new(&format!("{}##{}_id_{}", id, table_id, id))
                .span_all_columns(true)
                .build(ui);
            if ui.is_item_hovered() && ui.is_mouse_released(MouseButton::Right) {
                let code = encoder::generate_batch_link(LinkType::Item, *id);
                ui.set_clipboard_text(&code);
            }
            ui.table_next_column();
            ui.text(name);
            if show_char_count {
                ui.table_next_column();
                ui.text(name.chars().count().to_string());
            }
            if clicked {
                selected = Some(*id);
            }
        }
    }

    render_page_nav(ui, table_id, &mut page, total);
    (page, selected)
}

fn render_ingame_table_rows(
    ui: &Ui,
    table_id: &str,
    rows: &[db::ingame_items::SearchResult],
    link_type: LinkType,
    mut page: usize,
    show_char_count: bool,
) -> (usize, Option<u32>) {
    if rows.is_empty() {
        ui.text_disabled("No results.");
        return (0, None);
    }

    let total = rows.len();
    let total_pages = (total + RESULTS_PER_PAGE - 1) / RESULTS_PER_PAGE;
    if page >= total_pages {
        page = total_pages.saturating_sub(1);
    }

    let start = page * RESULTS_PER_PAGE;
    let end = (start + RESULTS_PER_PAGE).min(total);
    let mut selected = None;

    let flags = TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE;
    let cols = if show_char_count { 4 } else { 3 };
    if let Some(_t) = ui.begin_table_with_flags(table_id, cols, flags) {
        ui.table_setup_column("ID");
        ui.table_setup_column("Name");
        ui.table_setup_column("On API");
        if show_char_count {
            ui.table_setup_column("Chars");
        }
        ui.table_headers_row();

        for row in &rows[start..end] {
            ui.table_next_row();
            ui.table_next_column();
            let clicked = Selectable::new(&format!("{}##{}_id_{}", row.id, table_id, row.id))
                .span_all_columns(true)
                .build(ui);
            if ui.is_item_hovered() && ui.is_mouse_released(MouseButton::Right) {
                let code = encoder::generate_batch_link(link_type, row.id);
                ui.set_clipboard_text(&code);
            }
            ui.table_next_column();
            ui.text(&row.name);
            ui.table_next_column();
            ui.text(if row.in_api { "Yes" } else { "No" });
            if show_char_count {
                ui.table_next_column();
                ui.text(row.name.chars().count().to_string());
            }
            if clicked {
                selected = Some(row.id);
            }
        }
    }

    render_page_nav(ui, table_id, &mut page, total);
    (page, selected)
}

/// Renders a page of search results with navigation bar.
/// Returns Some(id) if the user clicked a result.
fn render_paged_results(
    ui: &Ui,
    label_suffix: &str,
    results: &[(u32, String)],
    page: &mut usize,
) -> Option<u32> {
    if results.is_empty() {
        return None;
    }

    let total = results.len();
    let total_pages = (total + RESULTS_PER_PAGE - 1) / RESULTS_PER_PAGE;

    // Clamp page
    if *page >= total_pages {
        *page = total_pages.saturating_sub(1);
    }

    let start = *page * RESULTS_PER_PAGE;
    let end = (start + RESULTS_PER_PAGE).min(total);

    let mut selected = None;
    for &(id, ref label) in &results[start..end] {
        if ui.small_button(&format!("{}##sel_{}{}", label, label_suffix, id)) {
            selected = Some(id);
        }
    }

    // Navigation bar
    if total_pages > 1 {
        if ui.small_button(&format!("|<< First##{}", label_suffix)) {
            *page = 0;
        }
        ui.same_line();
        if ui.small_button(&format!("< Prev##{}", label_suffix)) {
            *page = page.saturating_sub(1);
        }
        ui.same_line();
        ui.text(format!(
            "Page {} / {} ({} results)",
            *page + 1,
            total_pages,
            total,
        ));
        ui.same_line();
        if ui.small_button(&format!("Next >##{}", label_suffix)) {
            if *page + 1 < total_pages {
                *page += 1;
            }
        }
        ui.same_line();
        if ui.small_button(&format!("Last >>|##{}", label_suffix)) {
            *page = total_pages - 1;
        }
    } else {
        ui.text_disabled(format!("{} results", total));
    }

    selected
}

fn render_item_fields(ui: &Ui, fields: &mut ItemFields) {
    let (status, _, _, _) = db::get_status(LinkType::Item);
    let db_loaded = matches!(status, DbStatus::Loaded | DbStatus::Updating);
    let sort_names = ["Default", "ID Asc", "ID Desc", "Name A-Z", "Name Z-A"];

    // --- Item search ---
    if db_loaded {
        ui.text("Search Item (press Enter):");
        ui.set_next_item_width(200.0);
        let mut search = fields.item_search.clone();
        let submitted = InputText::new(ui, "##item_search", &mut search)
            .hint("Search items by name...")
            .flags(InputTextFlags::ENTER_RETURNS_TRUE)
            .build();
        fields.item_search = search.clone();
        let mut query_dirty = submitted;
        if submitted {
            fields.item_results = db::items::search_names(&search, fields.item_filter, usize::MAX);
            fields.item_page = 0;
        }

        ui.same_line();
        let filter_names: Vec<&str> = ItemFilter::ALL.iter().map(|f| f.name()).collect();
        ui.set_next_item_width(140.0);
        if ui.combo(
            "##item_filter",
            &mut fields.item_filter,
            &filter_names,
            |name| Cow::Borrowed(name),
        ) {
            query_dirty = true;
        }

        ui.same_line();
        ui.set_next_item_width(140.0);
        if ui.combo(
            "Sort##item_sort",
            &mut fields.item_sort_mode,
            &sort_names,
            |name| Cow::Borrowed(name),
        ) {
            query_dirty = true;
        }

        ui.set_next_item_width(120.0);
        if Drag::new("Min ID##item")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.item_min_id)
        {
            query_dirty = true;
        }
        ui.same_line();
        ui.set_next_item_width(120.0);
        if Drag::new("Max ID##item")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.item_max_id)
        {
            query_dirty = true;
        }

        ui.same_line();
        ui.checkbox("Show chars", &mut fields.item_show_char_count);

        if query_dirty {
            let mut rows =
                db::items::search_names(&fields.item_search, fields.item_filter, usize::MAX);
            let min_id = fields.item_min_id.max(0) as u32;
            let max_id = if fields.item_max_id > 0 {
                Some(fields.item_max_id as u32)
            } else {
                None
            };
            rows.retain(|(id, name)| {
                if *id < min_id {
                    return false;
                }
                if let Some(max_i) = max_id {
                    if *id > max_i {
                        return false;
                    }
                }
                let _ = name;
                true
            });
            match fields.item_sort_mode {
                1 => rows.sort_by_key(|(id, _)| *id),
                2 => rows.sort_by(|a, b| b.0.cmp(&a.0)),
                3 => rows.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase())),
                4 => rows.sort_by(|a, b| b.1.to_lowercase().cmp(&a.1.to_lowercase())),
                _ => {}
            }
            fields.item_results = rows;
            fields.item_page = 0;
        }

        let show_char_count = fields.item_show_char_count;
        let (new_page, selected_id) = render_item_table_rows(
            ui,
            "item_results",
            &fields.item_results,
            fields.item_page,
            show_char_count,
        );
        fields.item_page = new_page;
        if let Some(id) = selected_id {
            fields.id = id as i32;
            fields.item_results.clear();
            fields.item_search.clear();
        }
        ui.spacing();
    }

    // --- Manual ID + quantity ---
    ui.set_next_item_width(120.0);
    Drag::new("Item ID")
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut fields.id);

    ui.set_next_item_width(80.0);
    Drag::new("Quantity")
        .speed(0.1)
        .range(1, 255)
        .build(ui, &mut fields.quantity);

    ui.spacing();

    // --- Skin ---
    ui.checkbox("Apply Skin##skin", &mut fields.use_skin);
    if fields.use_skin {
        ui.same_line();
        ui.set_next_item_width(120.0);
        Drag::new("Skin ID")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.skin_id);
    }

    // --- First Upgrade ---
    ui.checkbox("First Upgrade##up1", &mut fields.use_upgrade1);
    if fields.use_upgrade1 {
        ui.same_line();
        ui.set_next_item_width(120.0);
        Drag::new("Upgrade 1 ID")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.upgrade1_id);

        if db_loaded {
            render_upgrade_search(
                ui,
                "##up1_search",
                "##up1_filter",
                "##up1_results",
                &mut fields.upgrade1_search,
                &mut fields.upgrade1_filter,
                &mut fields.upgrade1_results,
                &mut fields.upgrade1_id,
                &mut fields.upgrade1_page,
            );
        }
    }

    // --- Second Upgrade ---
    ui.checkbox("Second Upgrade##up2", &mut fields.use_upgrade2);
    if fields.use_upgrade2 {
        ui.same_line();
        ui.set_next_item_width(120.0);
        Drag::new("Upgrade 2 ID")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.upgrade2_id);

        if db_loaded {
            render_upgrade_search(
                ui,
                "##up2_search",
                "##up2_filter",
                "##up2_results",
                &mut fields.upgrade2_search,
                &mut fields.upgrade2_filter,
                &mut fields.upgrade2_results,
                &mut fields.upgrade2_id,
                &mut fields.upgrade2_page,
            );
        }
    }

    ui.spacing();

    if ui.button("Copy Chat Code##item") {
        let skin = if fields.use_skin && fields.skin_id > 0 {
            Some(fields.skin_id as u32)
        } else {
            None
        };
        let up1 = if fields.use_upgrade1 && fields.upgrade1_id > 0 {
            Some(fields.upgrade1_id as u32)
        } else {
            None
        };
        let up2 = if fields.use_upgrade2 && fields.upgrade2_id > 0 {
            Some(fields.upgrade2_id as u32)
        } else {
            None
        };

        fields.result = encoder::generate_item_link(
            fields.id.max(0) as u32,
            fields.quantity.clamp(1, 255) as u8,
            skin,
            up1,
            up2,
        );
        ui.set_clipboard_text(&fields.result);
    }

    render_result(ui, &fields.result);
}

fn render_upgrade_search(
    ui: &Ui,
    search_id: &str,
    filter_id: &str,
    results_id: &str,
    search: &mut String,
    filter_idx: &mut usize,
    results: &mut Vec<(u32, String)>,
    target_id: &mut i32,
    page: &mut usize,
) {
    ui.indent();
    ui.set_next_item_width(180.0);
    let mut search_buf = search.clone();
    let submitted = InputText::new(ui, search_id, &mut search_buf)
        .hint("Search upgrades (Enter)...")
        .flags(InputTextFlags::ENTER_RETURNS_TRUE)
        .build();
    *search = search_buf.clone();
    if submitted {
        *results = db::items::search_upgrades(&search_buf, *filter_idx, usize::MAX);
        *page = 0;
    }

    ui.same_line();
    let filter_names: Vec<&str> = UpgradeFilter::ALL.iter().map(|f| f.name()).collect();
    ui.set_next_item_width(130.0);
    ui.combo(filter_id, filter_idx, &filter_names, |name| {
        Cow::Borrowed(name)
    });

    if let Some(id) = render_paged_results(ui, results_id, results, page) {
        *target_id = id as i32;
        results.clear();
        search.clear();
    }
    ui.unindent();
}

fn render_searchable_fields(ui: &Ui, link_type: LinkType, fields: &mut SearchableFields) {
    let (status, _, _, _) = db::get_status(link_type);
    let db_loaded = matches!(status, DbStatus::Loaded | DbStatus::Updating);
    let sort_names = ["Default", "ID Asc", "ID Desc", "Name A-Z", "Name Z-A"];

    if db_loaded {
        ui.text(&format!("Search {} (press Enter):", link_type.name()));

        ui.set_next_item_width(200.0);
        let mut search = fields.search.clone();
        let submitted = InputText::new(ui, "##type_search", &mut search)
            .hint("Search by name...")
            .flags(InputTextFlags::ENTER_RETURNS_TRUE)
            .build();
        fields.search = search.clone();
        let mut query_dirty = submitted;

        // Show filter combo if this type supports filtering
        let filters = db::filter_names(link_type);
        if !filters.is_empty() {
            ui.same_line();
            ui.set_next_item_width(140.0);
            if ui.combo(
                "##type_filter",
                &mut fields.filter_index,
                &filters,
                |name| Cow::Borrowed(name),
            ) {
                query_dirty = true;
            }
        }

        ui.set_next_item_width(140.0);
        if ui.combo(
            "Sort##type_sort",
            &mut fields.sort_mode,
            &sort_names,
            |name| Cow::Borrowed(name),
        ) {
            query_dirty = true;
        }
        ui.same_line();
        ui.set_next_item_width(120.0);
        if Drag::new("Min ID##type")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.min_id)
        {
            query_dirty = true;
        }
        ui.same_line();
        ui.set_next_item_width(120.0);
        if Drag::new("Max ID##type")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut fields.max_id)
        {
            query_dirty = true;
        }

        if query_dirty {
            let mut rows = db::search(link_type, &search, fields.filter_index, usize::MAX);
            let min_id = fields.min_id.max(0) as u32;
            let max_id = if fields.max_id > 0 {
                Some(fields.max_id as u32)
            } else {
                None
            };
            rows.retain(|(id, _)| {
                if *id < min_id {
                    return false;
                }
                if let Some(max_i) = max_id {
                    if *id > max_i {
                        return false;
                    }
                }
                true
            });
            match fields.sort_mode {
                1 => rows.sort_by_key(|(id, _)| *id),
                2 => rows.sort_by(|a, b| b.0.cmp(&a.0)),
                3 => rows.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase())),
                4 => rows.sort_by(|a, b| b.1.to_lowercase().cmp(&a.1.to_lowercase())),
                _ => {}
            }
            fields.search_results = rows;
            fields.page = 0;
        }

        if let Some(id) = render_paged_results(ui, "type", &fields.search_results, &mut fields.page)
        {
            fields.id = id as i32;
            fields.search_results.clear();
            fields.search.clear();
        }
        ui.spacing();
    }

    // Manual ID input
    ui.set_next_item_width(120.0);
    Drag::new(format!("{} ID", link_type.name()))
        .speed(1.0)
        .range(0, i32::MAX)
        .build(ui, &mut fields.id);

    ui.spacing();

    let btn_label = format!("Copy {} Chat Code", link_type.name());
    if ui.button(&btn_label) {
        fields.result = encoder::generate_simple_link(link_type, fields.id.max(0) as u32);
        ui.set_clipboard_text(&fields.result);
    }

    render_result(ui, &fields.result);
}

fn render_result(ui: &Ui, result: &str) {
    if result.is_empty() {
        return;
    }

    ui.spacing();
    ui.separator();
    ui.spacing();

    let _color = ui.push_style_color(StyleColor::Text, [0.4, 0.8, 0.4, 1.0]);
    ui.text_wrapped(result);

    ui.same_line();
    if ui.small_button("Copy Chat Code##result") {
        ui.set_clipboard_text(result);
    }
}
