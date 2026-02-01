mod config;
mod encoder;
mod item_db;
mod ui;

use std::ffi::c_char;

use nexus::gui::{register_render, render, RenderType};
use nexus::keybind::register_keybind_with_string;
use nexus::AddonFlags;

use config::{load_user_config, save_user_config, RUNTIME_CONFIG};

nexus::export! {
    name: "Chat Link Generator",
    signature: -0x43484C4B,
    load,
    unload,
    flags: AddonFlags::None,
}

fn load() {
    load_user_config();
    item_db::load_item_db();

    register_keybind_with_string(
        "CLG_TOGGLE",
        toggle_window_keybind,
        "ALT+L",
    )
    .revert_on_unload();

    register_render(RenderType::Render, render!(|ui| {
        ui::main_window::render_main_window(ui);
    }))
    .revert_on_unload();

    register_render(RenderType::OptionsRender, render!(|ui| {
        ui::settings::render_settings(ui);
    }))
    .revert_on_unload();

    nexus::log::log(
        nexus::log::LogLevel::Info,
        "Chat Link Generator",
        "Addon loaded",
    );
}

fn unload() {
    save_user_config();
}

extern "C-unwind" fn toggle_window_keybind(_identifier: *const c_char, is_release: bool) {
    if !is_release {
        let mut cfg = RUNTIME_CONFIG.lock();
        cfg.show_main_window = !cfg.show_main_window;
    }
}
