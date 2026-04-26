use std::borrow::Cow;

use nexus::imgui::{Drag, Ui};

use crate::config::RUNTIME_CONFIG;
use crate::db;
use crate::encoder::LinkType;

pub fn render_settings(ui: &Ui) {
    let mut cfg = RUNTIME_CONFIG.lock();

    ui.text("Chat Link Generator Settings");
    ui.separator();

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();
    ui.set_next_item_width(180.0);
    ui.combo(
        "Default Type",
        &mut cfg.link_type_index,
        &type_names,
        |name| Cow::Borrowed(name),
    );

    ui.set_next_item_width(80.0);
    Drag::new("Default Batch Size")
        .speed(1.0)
        .range(1, 1000)
        .build(ui, &mut cfg.batch_size);

    ui.checkbox("Show ID Prefix", &mut cfg.show_id_prefix);
    ui.checkbox(
        "Auto-update In-Game Item DB on load",
        &mut cfg.auto_update_item_db_on_load,
    );

    // Drop cfg lock before calling db functions
    drop(cfg);

    ui.separator();
    ui.text("Database Management");
    ui.spacing();

    if ui.button("Rebuild All Databases") {
        db::log_debug("[settings] Rebuild All Databases clicked");
        db::rebuild_all();
    }

    ui.same_line();
    if ui.button("Clear All Caches") {
        db::log_debug("[settings] Clear All Caches clicked");
        db::clear_all_caches();
    }
}
