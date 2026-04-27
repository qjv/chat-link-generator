use std::borrow::Cow;
use std::time::{Duration, Instant};

use nexus::imgui::{
    Condition, Drag, InputText, InputTextFlags, MouseButton, Selectable, StyleColor,
    TableColumnFlags, TableColumnSetup, TableFlags, Ui, Window,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::catalog::{self, CatalogQuery, CatalogRecord, CatalogSort, SourceView};
use crate::config::RUNTIME_CONFIG;
use crate::db::{self, DbStatus};
use crate::encoder::LinkType;

const PAGE_SIZE: usize = 18;

struct WorkbenchState {
    selected_type: usize,
    source_view: usize,
    search: String,
    min_id: i32,
    max_id: i32,
    sort: usize,
    page: usize,
    selected_id: u32,
    rows: Vec<CatalogRecord>,
    cache_type: usize,
    cache_source_view: usize,
    cache_search: String,
    cache_min_id: i32,
    cache_max_id: i32,
    cache_sort: usize,
    copied_notice: String,
    copied_at: Option<Instant>,
}

impl Default for WorkbenchState {
    fn default() -> Self {
        Self {
            selected_type: 0,
            source_view: 0,
            search: String::new(),
            min_id: 0,
            max_id: 0,
            sort: 0,
            page: 0,
            selected_id: 0,
            rows: Vec::new(),
            cache_type: usize::MAX,
            cache_source_view: usize::MAX,
            cache_search: String::new(),
            cache_min_id: -1,
            cache_max_id: -1,
            cache_sort: usize::MAX,
            copied_notice: String::new(),
            copied_at: None,
        }
    }
}

struct ProbeState {
    selected_type: usize,
    id: i32,
    offset: i32,
    use_custom_content_type: bool,
    custom_content_type: i32,
    resolve_pending: bool,
    last_info: Option<db::ingame_items::ContentProbeDebugInfo>,
    last_error: String,
    last_resolve: Option<db::ingame_items::DebugResolveResult>,
    last_resolve_error: String,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self {
            selected_type: 0,
            id: 0,
            offset: 0x40,
            use_custom_content_type: false,
            custom_content_type: 0,
            resolve_pending: false,
            last_info: None,
            last_error: String::new(),
            last_resolve: None,
            last_resolve_error: String::new(),
        }
    }
}

static WORKBENCH_STATE: Lazy<Mutex<WorkbenchState>> =
    Lazy::new(|| Mutex::new(WorkbenchState::default()));
static PROBE_STATE: Lazy<Mutex<ProbeState>> = Lazy::new(|| Mutex::new(ProbeState::default()));

pub fn render_workbench_window(ui: &Ui) {
    drive_background_jobs();

    let mut open = {
        let cfg = RUNTIME_CONFIG.lock();
        cfg.show_main_window
    };
    if !open {
        return;
    }

    Window::new("Chat Link Workbench")
        .size([980.0, 680.0], Condition::FirstUseEver)
        .opened(&mut open)
        .build(ui, || {
            render_top_bar(ui);
            ui.separator();
            if let Some(_tabs) = ui.tab_bar("##workbench_tabs") {
                if let Some(_tab) = ui.tab_item("Browse") {
                    render_browse_tab(ui);
                }
                if let Some(_tab) = ui.tab_item("Jobs") {
                    render_jobs_tab(ui);
                }
                if let Some(_tab) = ui.tab_item("Probe") {
                    render_probe_tab(ui);
                }
            }
        });

    let mut cfg = RUNTIME_CONFIG.lock();
    cfg.show_main_window = open;
}

fn drive_background_jobs() {
    db::ingame_items::maybe_auto_update_on_load();
    let (status, _, _, _) = db::ingame_items::get_status();
    if matches!(status, DbStatus::Loading | DbStatus::Updating)
        || db::ingame_items::has_pending_debug_resolve()
    {
        db::ingame_items::tick();
    }
}

fn render_top_bar(ui: &Ui) {
    let link_type = {
        let st = WORKBENCH_STATE.lock();
        LinkType::ALL[st.selected_type]
    };
    let status = catalog::source_status(link_type);
    render_status_chip(ui, "API", status.api);
    ui.same_line();
    render_status_chip(ui, "Game", status.game);

    if let Some((done, total, added, current_type)) =
        db::ingame_items::get_build_missing_game_data_progress()
    {
        ui.same_line();
        ui.text_disabled(format!(
            "Job: {} {} / {} added {}",
            current_type,
            format_number(done),
            format_number(total),
            format_number(added)
        ));
    }

    let copied_notice = {
        let mut st = WORKBENCH_STATE.lock();
        if st
            .copied_at
            .map(|at| at.elapsed() <= Duration::from_secs(3))
            .unwrap_or(false)
        {
            Some(st.copied_notice.clone())
        } else {
            st.copied_at = None;
            None
        }
    };
    if let Some(notice) = copied_notice {
        ui.same_line();
        ui.text_disabled(notice);
    }
}

fn copy_to_clipboard(ui: &Ui, text: &str, what: &str) {
    ui.set_clipboard_text(text);
    let mut st = WORKBENCH_STATE.lock();
    st.copied_notice = format!("Copied {}", what);
    st.copied_at = Some(Instant::now());
}

fn render_status_chip(
    ui: &Ui,
    label: &str,
    status: (DbStatus, usize, String, Option<(usize, usize)>),
) {
    let (state, count, error, progress) = status;
    match state {
        DbStatus::NotLoaded => ui.text_disabled(format!("{}: not loaded", label)),
        DbStatus::Loading => ui.text(format!(
            "{}: loading {}",
            label,
            progress_text(progress).unwrap_or_default()
        )),
        DbStatus::Loaded => ui.text(format!("{}: {}", label, format_number(count))),
        DbStatus::Updating => ui.text(format!(
            "{}: updating {}",
            label,
            progress_text(progress).unwrap_or_default()
        )),
        DbStatus::Error => {
            let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.35, 0.35, 1.0]);
            ui.text(format!("{}: {}", label, error));
        }
    }
}

fn render_browse_tab(ui: &Ui) {
    let mut refresh = false;
    let (api_action, game_action, names_action) = {
        let mut st = WORKBENCH_STATE.lock();
        let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();
        ui.set_next_item_width(190.0);
        if ui.combo(
            "Type##browse_type",
            &mut st.selected_type,
            &type_names,
            |name| Cow::Borrowed(name),
        ) {
            st.page = 0;
        }

        ui.same_line();
        let source_names: Vec<&str> = SourceView::ALL.iter().map(|s| s.name()).collect();
        ui.set_next_item_width(150.0);
        if ui.combo(
            "Source##browse_source",
            &mut st.source_view,
            &source_names,
            |name| Cow::Borrowed(name),
        ) {
            st.page = 0;
        }

        ui.same_line();
        let sort_names: Vec<&str> = CatalogSort::ALL.iter().map(|s| s.name()).collect();
        ui.set_next_item_width(130.0);
        if ui.combo("Sort##browse_sort", &mut st.sort, &sort_names, |name| {
            Cow::Borrowed(name)
        }) {
            st.page = 0;
        }

        ui.set_next_item_width(280.0);
        let mut search = st.search.clone();
        let submitted = InputText::new(ui, "Search##browse_search", &mut search)
            .hint("Name or ID")
            .flags(InputTextFlags::ENTER_RETURNS_TRUE)
            .build();
        if st.search != search {
            st.search = search;
            st.page = 0;
        }
        refresh |= submitted;

        ui.same_line();
        ui.set_next_item_width(90.0);
        if Drag::new("Min##browse_min")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut st.min_id)
        {
            st.page = 0;
        }
        ui.same_line();
        ui.set_next_item_width(90.0);
        if Drag::new("Max##browse_max")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut st.max_id)
        {
            st.page = 0;
        }

        if ui.small_button("Refresh##browse") {
            refresh = true;
        }
        ui.same_line();
        let api_action = ui.small_button("API##browse_api");
        ui.same_line();
        let game_action = ui.small_button("Game##browse_game");
        ui.same_line();
        let names_action = ui.small_button("Names##browse_names");
        ui.same_line();
        ui.text_disabled("Ctrl-click API/Game/Names to rebuild from zero");
        (api_action, game_action, names_action)
    };

    let link_type = {
        let st = WORKBENCH_STATE.lock();
        LinkType::ALL[st.selected_type]
    };
    let ctrl = ui.io().key_ctrl;

    if api_action {
        if ctrl {
            db::rebuild(link_type);
        } else {
            db::update(link_type);
        }
        refresh = true;
    }
    if game_action {
        if link_type == LinkType::Item {
            let (status, count, _, _) = db::ingame_items::get_status();
            if ctrl || matches!(status, DbStatus::NotLoaded) || count == 0 {
                db::ingame_items::rebuild();
            } else {
                db::ingame_items::update();
            }
        } else {
            db::ingame_items::start_build_game_data_for_link_type(link_type, ctrl);
        }
        refresh = true;
    }
    if names_action {
        if link_type == LinkType::Item {
            if ctrl {
                db::ingame_items::full_rebuild_names_from_hashes(false, false);
            } else {
                db::ingame_items::parse_names_from_hashes(false, false);
            }
        } else if link_type == LinkType::Map {
            if ctrl {
                db::ingame_items::full_rebuild_game_type_names_from_hashes(link_type);
                db::ingame_items::full_rebuild_map_names_from_hashes();
            } else {
                db::ingame_items::parse_game_type_names_from_hashes(link_type);
                db::ingame_items::parse_map_names_from_hashes();
            }
        } else if ctrl {
            db::ingame_items::full_rebuild_game_type_names_from_hashes(link_type);
        } else {
            db::ingame_items::parse_game_type_names_from_hashes(link_type);
        }
        refresh = true;
    }

    catalog::ensure_sources_loaded(link_type);
    refresh_cached_rows(refresh);
    render_browser_table(ui);
    render_selected_detail(ui);
}

fn refresh_cached_rows(force: bool) {
    let mut st = WORKBENCH_STATE.lock();
    let stale = force
        || st.cache_type != st.selected_type
        || st.cache_source_view != st.source_view
        || st.cache_search != st.search
        || st.cache_min_id != st.min_id
        || st.cache_max_id != st.max_id
        || st.cache_sort != st.sort;
    if !stale {
        return;
    }

    let link_type = LinkType::ALL[st.selected_type];
    let query = CatalogQuery {
        link_type,
        source_view: SourceView::ALL
            .get(st.source_view)
            .copied()
            .unwrap_or(SourceView::Merged),
        search: st.search.clone(),
        min_id: st.min_id.max(0) as u32,
        max_id: if st.max_id > 0 {
            Some(st.max_id as u32)
        } else {
            None
        },
        sort: CatalogSort::ALL
            .get(st.sort)
            .copied()
            .unwrap_or(CatalogSort::IdAsc),
    };
    st.rows = catalog::query_records(&query);
    st.cache_type = st.selected_type;
    st.cache_source_view = st.source_view;
    st.cache_search = st.search.clone();
    st.cache_min_id = st.min_id;
    st.cache_max_id = st.max_id;
    st.cache_sort = st.sort;
    st.page = st.page.min(total_pages(st.rows.len()).saturating_sub(1));
}

fn render_browser_table(ui: &Ui) {
    let mut clicked_id = None;
    let mut copy_link = None;
    {
        let mut st = WORKBENCH_STATE.lock();
        let total = st.rows.len();
        let pages = total_pages(total);
        if st.page >= pages {
            st.page = pages.saturating_sub(1);
        }
        let start = st.page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(total);

        ui.text(format!("Rows: {}", format_number(total)));
        ui.same_line();
        render_page_nav(ui, "browse", &mut st.page, total);
        handle_page_wheel(ui, &mut st.page, total);

        let visible = &st.rows[start..end];
        let widths = catalog_table_widths(ui, visible);
        let flags = TableFlags::BORDERS_INNER_V
            | TableFlags::ROW_BG
            | TableFlags::RESIZABLE
            | TableFlags::SIZING_STRETCH_PROP;
        if let Some(_table) = ui.begin_table_with_flags("##catalog_table", 6, flags) {
            setup_fixed_column(ui, "ID", widths.id);
            setup_fixed_column(ui, "Type", widths.category);
            setup_fixed_column(ui, "API", widths.api);
            ui.table_setup_column_with(TableColumnSetup {
                name: "Game",
                flags: TableColumnFlags::WIDTH_STRETCH,
                init_width_or_weight: 1.0,
                user_id: 0.into(),
            });
            setup_fixed_column(ui, "State", widths.state);
            setup_fixed_column(ui, "Link", widths.link);
            ui.table_headers_row();

            for row in visible {
                ui.table_next_row();
                ui.table_next_column();
                let selected = st.selected_id == row.id;
                let clicked = Selectable::new(&format!("{}##catalog_id_{}", row.id, row.id))
                    .selected(selected)
                    .build(ui);
                if ui.is_item_hovered() && ui.is_mouse_released(MouseButton::Right) {
                    copy_link = Some(row.chat_link.clone());
                }
                if clicked {
                    clicked_id = Some(row.id);
                }

                ui.table_next_column();
                ui.text(&row.category);
                ui.table_next_column();
                ui.text(row.api_name.as_deref().unwrap_or("-"));
                ui.table_next_column();
                ui.text(
                    row.game_name
                        .as_deref()
                        .filter(|n| !n.is_empty())
                        .unwrap_or("-"),
                );
                ui.table_next_column();
                render_comparison_label(ui, row.comparison);
                ui.table_next_column();
                if ui.small_button(&format!("Copy##copy_link_{}", row.id)) {
                    copy_link = Some(row.chat_link.clone());
                }
            }
        }

        if let Some(id) = clicked_id {
            st.selected_id = id;
        }
    }

    if let Some(link) = copy_link {
        copy_to_clipboard(ui, &link, "chat link");
    }
}

fn render_selected_detail(ui: &Ui) {
    let selected = {
        let st = WORKBENCH_STATE.lock();
        st.rows.iter().find(|r| r.id == st.selected_id).cloned()
    };
    let Some(row) = selected else {
        return;
    };

    ui.separator();
    ui.text(format!("Selected: {} {}", row.id, row.display_name()));
    ui.same_line();
    if ui.small_button("Copy Link##detail_link") {
        copy_to_clipboard(ui, &row.chat_link, "chat link");
    }
    ui.same_line();
    if ui.small_button("Copy ID##detail_id") {
        copy_to_clipboard(ui, &row.id.to_string(), "id");
    }
    if let Some(name) = row
        .game_name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .or(row.api_name.as_deref())
    {
        ui.same_line();
        if ui.small_button("Copy Name##detail_name") {
            copy_to_clipboard(ui, name, "name");
        }
    }
    ui.same_line();
    if ui.small_button("Probe##detail_probe") {
        let mut probe = PROBE_STATE.lock();
        probe.selected_type = LinkType::ALL
            .iter()
            .position(|t| *t == row.link_type)
            .unwrap_or(0);
        probe.id = row.id as i32;
    }

    let mut link_preview = row.chat_link.clone();
    ui.set_next_item_width(-1.0);
    InputText::new(ui, "Chat Link##detail_link_text", &mut link_preview)
        .flags(InputTextFlags::READ_ONLY)
        .build();
    ui.text(format!(
        "API: {}",
        row.api_name.as_deref().unwrap_or("<missing>")
    ));
    ui.text(format!(
        "Game: {}",
        row.game_name.as_deref().unwrap_or("<missing>")
    ));
    ui.text(format!("Comparison: {}", row.comparison.label()));

    if let Some(extra) = db::ingame_items::get_game_data_entry_for_link_type(row.link_type, row.id)
    {
        ui.text_disabled(format!(
            "Hashes: name={} description={}",
            extra.name_hash, extra.description_hash
        ));
        if row.link_type == LinkType::Wardrobe {
            ui.text_disabled(format!(
                "Skin metadata: type={} rarity={} flags=0x{:08X}",
                extra.skin_type_code, extra.skin_rarity_code, extra.skin_flags_code
            ));
        }
        if row.link_type == LinkType::Map {
            ui.text_disabled(format!(
                "POI metadata: type={} map_hash={} map={}",
                extra.poi_type_code,
                extra.map_name_hash,
                if extra.map_name.is_empty() {
                    "<unresolved>"
                } else {
                    &extra.map_name
                }
            ));
        }
    }
}

struct CatalogTableWidths {
    id: f32,
    category: f32,
    api: f32,
    state: f32,
    link: f32,
}

fn catalog_table_widths(ui: &Ui, rows: &[CatalogRecord]) -> CatalogTableWidths {
    let text_width = |text: &str| ui.calc_text_size(text)[0] + 18.0;
    let mut id = text_width("0000000");
    let mut category = text_width("Type");
    let mut api = text_width("API");
    let mut state = text_width("Different");
    for row in rows {
        id = id.max(text_width(&row.id.to_string()));
        category = category.max(text_width(&row.category));
        api = api.max(text_width(row.api_name.as_deref().unwrap_or("-")));
        state = state.max(text_width(row.comparison.label()));
    }
    CatalogTableWidths {
        id,
        category,
        api,
        state,
        link: text_width("Copy") + 18.0,
    }
}

fn setup_fixed_column(ui: &Ui, name: &'static str, width: f32) {
    ui.table_setup_column_with(TableColumnSetup {
        name,
        flags: TableColumnFlags::WIDTH_FIXED,
        init_width_or_weight: width,
        user_id: 0.into(),
    });
}

fn render_jobs_tab(ui: &Ui) {
    let link_type = {
        let st = WORKBENCH_STATE.lock();
        LinkType::ALL[st.selected_type]
    };

    if ui.small_button("Load All API##jobs") {
        db::ensure_all_loaded();
    }
    ui.same_line();
    if ui.small_button("Update All API##jobs") {
        db::update_all();
    }
    ui.same_line();
    if ui.small_button("Clear All Caches##jobs") {
        db::clear_all_caches();
    }

    ui.separator();
    let status = catalog::source_status(link_type);
    ui.text(format!("Selected type: {}", link_type.name()));
    render_status_line(ui, "API", status.api.clone());
    render_status_line(ui, "Game", status.game.clone());
    {
        let mut cfg = RUNTIME_CONFIG.lock();
        ui.set_next_item_width(80.0);
        Drag::new("Name Decodes/Tick##jobs")
            .speed(1.0)
            .range(1, 64)
            .build(ui, &mut cfg.name_decodes_per_tick);
    }
    ui.text_disabled(
        "All-type game-memory builds are disabled. Use Browse > Update Game for one selected type.",
    );

    if let Some((done, total, added, current_type)) =
        db::ingame_items::get_build_missing_game_data_progress()
    {
        ui.separator();
        ui.text(format!(
            "Game data job: {} {} / {} added {}",
            current_type,
            format_number(done),
            format_number(total),
            format_number(added)
        ));
        if db::ingame_items::is_build_missing_game_data_paused() {
            if ui.small_button("Resume Game Job##jobs") {
                db::ingame_items::set_build_missing_game_data_paused(false);
            }
        } else if ui.small_button("Pause Game Job##jobs") {
            db::ingame_items::set_build_missing_game_data_paused(true);
        }
    }

    if db::ingame_items::is_name_parse_paused() {
        if ui.small_button("Resume Name Parse##jobs") {
            db::ingame_items::set_name_parse_paused(false);
        }
    } else if matches!(status.game.0, DbStatus::Updating)
        && ui.small_button("Pause Name Parse##jobs")
    {
        db::ingame_items::set_name_parse_paused(true);
    }
}

fn render_probe_tab(ui: &Ui) {
    if let Some(res) = db::ingame_items::consume_debug_resolve_result() {
        let mut st = PROBE_STATE.lock();
        st.resolve_pending = false;
        match res {
            Ok(value) => {
                st.last_resolve = Some(value);
                st.last_resolve_error.clear();
            }
            Err(err) => {
                st.last_resolve = None;
                st.last_resolve_error = err;
            }
        }
    }

    let mut run_probe = false;
    let mut run_subdef_probe = false;
    let mut run_resolve = false;
    {
        let mut st = PROBE_STATE.lock();
        let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();
        ui.set_next_item_width(190.0);
        ui.combo(
            "Type##probe_type",
            &mut st.selected_type,
            &type_names,
            |name| Cow::Borrowed(name),
        );
        ui.same_line();
        ui.set_next_item_width(110.0);
        Drag::new("ID##probe_id")
            .speed(1.0)
            .range(0, i32::MAX)
            .build(ui, &mut st.id);
        ui.same_line();
        ui.set_next_item_width(90.0);
        Drag::new("Offset##probe_offset")
            .speed(1.0)
            .range(0, 0x300)
            .build(ui, &mut st.offset);

        ui.same_line();
        ui.checkbox("Custom Type##probe_custom", &mut st.use_custom_content_type);
        if st.use_custom_content_type {
            ui.same_line();
            ui.set_next_item_width(100.0);
            Drag::new("Content##probe_content_type")
                .speed(1.0)
                .range(0, 10000)
                .build(ui, &mut st.custom_content_type);
        }

        if ui.small_button("Probe Offsets##probe_run") {
            run_probe = true;
        }
        ui.same_line();
        if ui.small_button("Probe Subdef##probe_subdef") {
            run_subdef_probe = true;
        }
        ui.same_line();
        if ui.small_button("Resolve Offset##probe_resolve") {
            run_resolve = true;
        }
    }

    if run_probe {
        let mut st = PROBE_STATE.lock();
        let link_type = LinkType::ALL[st.selected_type];
        let content_type = if st.use_custom_content_type {
            Some(st.custom_content_type.max(0) as u32)
        } else {
            None
        };
        match db::ingame_items::debug_probe_content_for_content_type(
            link_type,
            st.id.max(0) as u32,
            content_type,
        ) {
            Ok(info) => {
                st.last_info = Some(info);
                st.last_error.clear();
            }
            Err(err) => {
                st.last_info = None;
                st.last_error = err;
            }
        }
    }

    if run_subdef_probe {
        let mut st = PROBE_STATE.lock();
        let link_type = LinkType::ALL[st.selected_type];
        let content_type = if st.use_custom_content_type {
            Some(st.custom_content_type.max(0) as u32)
        } else {
            None
        };
        match db::ingame_items::debug_probe_item_subdef_for_content_type(
            link_type,
            st.id.max(0) as u32,
            content_type,
        ) {
            Ok(info) => {
                st.last_info = Some(info);
                st.last_error.clear();
            }
            Err(err) => {
                st.last_info = None;
                st.last_error = err;
            }
        }
    }

    if run_resolve {
        let mut st = PROBE_STATE.lock();
        let link_type = LinkType::ALL[st.selected_type];
        let content_type = if st.use_custom_content_type {
            Some(st.custom_content_type.max(0) as u32)
        } else {
            None
        };
        match db::ingame_items::queue_debug_resolve_offset_for_content_type(
            link_type,
            st.id.max(0) as u32,
            content_type,
            st.offset.max(0) as usize,
        ) {
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

    render_probe_results(ui);
}

fn render_probe_results(ui: &Ui) {
    let (info, error, pending, resolve, resolve_error, selected_offset) = {
        let st = PROBE_STATE.lock();
        (
            st.last_info.clone(),
            st.last_error.clone(),
            st.resolve_pending,
            st.last_resolve.clone(),
            st.last_resolve_error.clone(),
            st.offset.max(0) as usize,
        )
    };

    if !error.is_empty() {
        let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.35, 0.35, 1.0]);
        ui.text(error);
    }
    if pending {
        ui.text_disabled("Resolve pending");
    } else if !resolve_error.is_empty() {
        let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.35, 0.35, 1.0]);
        ui.text(format!("Resolve error: {}", resolve_error));
    } else if let Some(result) = resolve {
        let copy_text = result
            .decoded_text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&result.coded_text)
            .to_string();
        ui.text(format!(
            "Resolved 0x{:X}: {} / 0x{:08X}",
            result.offset, result.raw_u32, result.raw_u32
        ));
        ui.same_line();
        if ui.small_button("Copy Text##probe_resolved_copy") {
            copy_to_clipboard(ui, &copy_text, "resolved text");
        }
        if let Some(text) = result.decoded_text {
            ui.text_wrapped(text);
        } else {
            ui.text_wrapped(result.coded_text);
        }
    }

    let Some(info) = info else {
        return;
    };

    ui.separator();
    ui.text(format!(
        "{} #{} content={} ptr=0x{:X}",
        info.link_type.name(),
        info.id,
        info.resolved_content_type,
        info.content_ptr
    ));
    if let Some(row) = info.rows.iter().find(|r| r.offset == selected_offset) {
        ui.text_disabled(format!(
            "Selected 0x{:X}: {} / 0x{:08X}",
            row.offset, row.raw_u32, row.raw_u32
        ));
    }

    let flags = TableFlags::BORDERS_INNER_V | TableFlags::ROW_BG | TableFlags::RESIZABLE;
    let mut resolve_hash = None;
    if let Some(_table) = ui.begin_table_with_flags("##probe_offsets", 5, flags) {
        ui.table_setup_column("Offset");
        ui.table_setup_column("Dec");
        ui.table_setup_column("Hex");
        ui.table_setup_column("Hash");
        ui.table_setup_column("Resolve");
        ui.table_headers_row();
        let mut clicked_offset = None;
        for row in &info.rows {
            ui.table_next_row();
            ui.table_next_column();
            if Selectable::new(&format!("0x{:X}##probe_off_{:X}", row.offset, row.offset)).build(ui)
            {
                clicked_offset = Some(row.offset);
            }
            ui.table_next_column();
            ui.text(row.raw_u32.to_string());
            ui.table_next_column();
            ui.text(format!("0x{:08X}", row.raw_u32));
            ui.table_next_column();
            if row.is_hash_candidate {
                ui.text_colored([0.45, 0.9, 0.55, 1.0], "Yes");
            } else {
                ui.text_disabled("-");
            }
            ui.table_next_column();
            if row.is_hash_candidate
                && ui.small_button(&format!("Decode##probe_decode_{:X}", row.offset))
            {
                resolve_hash = Some((row.offset, row.raw_u32, info.content_ptr));
            }
        }
        if let Some(offset) = clicked_offset {
            PROBE_STATE.lock().offset = offset as i32;
        }
    }

    if !info.subdef_rows.is_empty() {
        ui.separator();
        ui.text(format!("Subdef ptr=0x{:X}", info.subdef_ptr));
        if let Some(_table) = ui.begin_table_with_flags("##probe_subdef_offsets", 5, flags) {
            ui.table_setup_column("Offset");
            ui.table_setup_column("Dec");
            ui.table_setup_column("Hex");
            ui.table_setup_column("Hash");
            ui.table_setup_column("Resolve");
            ui.table_headers_row();
            for row in &info.subdef_rows {
                ui.table_next_row();
                ui.table_next_column();
                if Selectable::new(&format!(
                    "0x{:X}##probe_subdef_off_{:X}",
                    row.offset, row.offset
                ))
                .build(ui)
                {
                    PROBE_STATE.lock().offset = row.offset as i32;
                }
                ui.table_next_column();
                ui.text(row.raw_u32.to_string());
                ui.table_next_column();
                ui.text(format!("0x{:08X}", row.raw_u32));
                ui.table_next_column();
                if row.is_hash_candidate {
                    ui.text_colored([0.45, 0.9, 0.55, 1.0], "Yes");
                } else {
                    ui.text_disabled("-");
                }
                ui.table_next_column();
                if row.is_hash_candidate
                    && ui.small_button(&format!("Decode##probe_subdef_decode_{:X}", row.offset))
                {
                    resolve_hash = Some((row.offset, row.raw_u32, info.subdef_ptr));
                }
            }
        }
    }

    if let Some((offset, raw_u32, source_ptr)) = resolve_hash {
        match db::ingame_items::queue_debug_resolve_hash(
            info.link_type,
            info.id,
            offset,
            raw_u32,
            Some(source_ptr),
        ) {
            Ok(_) => {
                let mut st = PROBE_STATE.lock();
                st.resolve_pending = true;
                st.last_resolve = None;
                st.last_resolve_error.clear();
            }
            Err(err) => {
                let mut st = PROBE_STATE.lock();
                st.resolve_pending = false;
                st.last_resolve = None;
                st.last_resolve_error = err;
            }
        }
    }
}

fn render_status_line(
    ui: &Ui,
    label: &str,
    status: (DbStatus, usize, String, Option<(usize, usize)>),
) {
    let (state, count, error, progress) = status;
    match state {
        DbStatus::NotLoaded => ui.text_disabled(format!("{}: not loaded", label)),
        DbStatus::Loading => ui.text(format!(
            "{}: loading {}",
            label,
            progress_text(progress).unwrap_or_default()
        )),
        DbStatus::Loaded => ui.text(format!("{}: {} records", label, format_number(count))),
        DbStatus::Updating => ui.text(format!(
            "{}: updating {}",
            label,
            progress_text(progress).unwrap_or_default()
        )),
        DbStatus::Error => {
            let _c = ui.push_style_color(StyleColor::Text, [1.0, 0.35, 0.35, 1.0]);
            ui.text(format!("{}: {}", label, error));
        }
    }
}

fn render_comparison_label(ui: &Ui, comparison: catalog::ComparisonState) {
    match comparison {
        catalog::ComparisonState::Same => {
            ui.text_colored([0.45, 0.9, 0.55, 1.0], comparison.label())
        }
        catalog::ComparisonState::Different => {
            ui.text_colored([1.0, 0.55, 0.55, 1.0], comparison.label())
        }
        catalog::ComparisonState::ApiOnly | catalog::ComparisonState::GameOnly => {
            ui.text_colored([0.95, 0.78, 0.35, 1.0], comparison.label())
        }
        catalog::ComparisonState::Unknown => ui.text_disabled(comparison.label()),
    }
}

fn render_page_nav(ui: &Ui, label: &str, page: &mut usize, total: usize) {
    let pages = total_pages(total);
    if pages <= 1 {
        return;
    }
    if ui.small_button(&format!("First##{}_first", label)) {
        *page = 0;
    }
    ui.same_line();
    if ui.small_button(&format!("Prev##{}_prev", label)) {
        *page = page.saturating_sub(1);
    }
    ui.same_line();
    ui.text_disabled(format!("Page {} / {}", *page + 1, pages));
    ui.same_line();
    if ui.small_button(&format!("Next##{}_next", label)) && *page + 1 < pages {
        *page += 1;
    }
    ui.same_line();
    if ui.small_button(&format!("Last##{}_last", label)) {
        *page = pages.saturating_sub(1);
    }
}

fn handle_page_wheel(ui: &Ui, page: &mut usize, total: usize) {
    let wheel = ui.io().mouse_wheel;
    if wheel == 0.0 || !ui.is_window_hovered() {
        return;
    }
    let pages = total_pages(total);
    if pages <= 1 {
        return;
    }
    if wheel < 0.0 && *page + 1 < pages {
        *page += 1;
    } else if wheel > 0.0 {
        *page = page.saturating_sub(1);
    }
}

fn progress_text(progress: Option<(usize, usize)>) -> Option<String> {
    let (done, total) = progress?;
    Some(format!(
        "{} / {}",
        format_number(done),
        format_number(total)
    ))
}

fn total_pages(total: usize) -> usize {
    total.div_ceil(PAGE_SIZE).max(1)
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
