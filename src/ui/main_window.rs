use std::borrow::Cow;

use nexus::imgui::{ChildWindow, Condition, Drag, InputText, InputTextFlags, MouseButton, StyleColor, Ui, Window};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::config::RUNTIME_CONFIG;
use crate::encoder::{self, LinkType};
use crate::db::{self, DbStatus};
use crate::db::items::{ItemFilter, UpgradeFilter};

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

const RESULTS_PER_PAGE: usize = 20;

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

// --- Batch helpers ---

fn generate_batch(link_type: LinkType, start_id: u32, batch_size: u32, show_id_prefix: bool) -> Batch {
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

fn generate_initial_batches(link_type_index: usize, start_id: i32, batch_size: i32, show_id_prefix: bool) {
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
    let mut open = {
        let cfg = RUNTIME_CONFIG.lock();
        cfg.show_main_window
    };

    if !open {
        return;
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
        if let Some(_tab) = ui.tab_item("Individual Link") {
            render_individual_tab(ui);
        }
    }
}

fn render_batch_tab(ui: &Ui) {
    let (mut link_type_index, mut start_id, mut batch_size, mut show_id_prefix) = {
        let cfg = RUNTIME_CONFIG.lock();
        (cfg.link_type_index, cfg.start_id, cfg.batch_size, cfg.show_id_prefix)
    };

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    let prev_type = link_type_index;
    ui.set_next_item_width(180.0);
    if ui.combo("Type", &mut link_type_index, &type_names, |name| Cow::Borrowed(name)) {
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
        ui.text_disabled("Press Generate to create batches. Scroll down for more, scroll up for previous.");
        return;
    }

    let avail = ui.content_region_avail();

    if ui.button("< Previous Batch") {
        prepend_backward_batch(show_id_prefix);
    }
    ui.same_line();
    if ui.button("Next Batch >") {
        append_forward_batch(show_id_prefix);
    }

    let child_h = avail[1] - 30.0;
    ChildWindow::new("##batches")
        .size([0.0, child_h])
        .build(ui, || {
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
            drop(state);

            let scroll_y = ui.scroll_y();
            let scroll_max = ui.scroll_max_y();
            if scroll_max > 0.0 && scroll_y >= scroll_max - 20.0 {
                append_forward_batch(show_id_prefix);
            }
            if scroll_y <= 1.0 && ui.is_mouse_dragging(MouseButton::Left) {
                prepend_backward_batch(show_id_prefix);
            }
        });
}

fn render_individual_tab(ui: &Ui) {
    let mut state = INDIVIDUAL_STATE.lock();

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();

    ui.set_next_item_width(180.0);
    let prev = state.prev_selected_type;
    ui.combo("Type##individual", &mut state.selected_type, &type_names, |name| Cow::Borrowed(name));

    let link_type = LinkType::ALL[state.selected_type];

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
        if submitted {
            fields.item_results = db::items::search(&search, fields.item_filter, usize::MAX);
            fields.item_page = 0;
        }

        ui.same_line();
        let filter_names: Vec<&str> = ItemFilter::ALL.iter().map(|f| f.name()).collect();
        ui.set_next_item_width(140.0);
        ui.combo("##item_filter", &mut fields.item_filter, &filter_names, |name| Cow::Borrowed(name));

        if let Some(id) = render_paged_results(ui, "item", &fields.item_results, &mut fields.item_page) {
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

    if ui.button("Generate Item Link") {
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
    ui.combo(filter_id, filter_idx, &filter_names, |name| Cow::Borrowed(name));

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

    if db_loaded {
        ui.text(&format!("Search {} (press Enter):", link_type.name()));

        ui.set_next_item_width(200.0);
        let mut search = fields.search.clone();
        let submitted = InputText::new(ui, "##type_search", &mut search)
            .hint("Search by name...")
            .flags(InputTextFlags::ENTER_RETURNS_TRUE)
            .build();
        fields.search = search.clone();

        // Show filter combo if this type supports filtering
        let filters = db::filter_names(link_type);
        if !filters.is_empty() {
            ui.same_line();
            ui.set_next_item_width(140.0);
            ui.combo("##type_filter", &mut fields.filter_index, &filters, |name| Cow::Borrowed(name));
        }

        if submitted {
            fields.search_results = db::search(link_type, &search, fields.filter_index, usize::MAX);
            fields.page = 0;
        }

        if let Some(id) = render_paged_results(ui, "type", &fields.search_results, &mut fields.page) {
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

    let btn_label = format!("Generate {} Link", link_type.name());
    if ui.button(&btn_label) {
        fields.result = encoder::generate_simple_link(link_type, fields.id.max(0) as u32);
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
    if ui.small_button("Copy##result") {
        ui.set_clipboard_text(result);
    }
}
