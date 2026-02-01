use std::borrow::Cow;

use nexus::imgui::{Drag, Ui};

use crate::config::RUNTIME_CONFIG;
use crate::encoder::LinkType;

pub fn render_settings(ui: &Ui) {
    let mut cfg = RUNTIME_CONFIG.lock();

    ui.text("Chat Link Generator Settings");
    ui.separator();

    let type_names: Vec<&str> = LinkType::ALL.iter().map(|t| t.name()).collect();
    ui.set_next_item_width(180.0);
    ui.combo("Default Type", &mut cfg.link_type_index, &type_names, |name| Cow::Borrowed(name));

    ui.set_next_item_width(80.0);
    Drag::new("Default Batch Size")
        .speed(1.0)
        .range(1, 1000)
        .build(ui, &mut cfg.batch_size);

    ui.checkbox("Show ID Prefix", &mut cfg.show_id_prefix);
}
