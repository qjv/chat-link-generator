mod game;

use nexus::gui::{register_render, render, RenderType};
use nexus::imgui::{Condition, Ui, Window, WindowFlags};
use nexus::quick_access::{add_quick_access, remove_quick_access};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

const QA_ID: &str = "LEGENDARY_MANAGER_QA";
const TOGGLE_KEY: &str = "LEGENDARY_MANAGER_TOGGLE";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotChoice {
    pub slot_index: u32,
    pub item_id: u32,
    pub item_def_ptr: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegendaryPreset {
    pub name: String,
    pub choices: Vec<SlotChoice>,
}

#[derive(Default)]
struct AppState {
    visible: bool,
    last_status: String,
    presets: Vec<LegendaryPreset>,
    live_slots: Vec<game::LiveSlotEntry>,
}

static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));
static LAST_PLAYABLE: AtomicBool = AtomicBool::new(false);

nexus::export! {
    name: "Legendary Manager",
    signature: -0x4C45474D,
    load,
    unload,
    flags: nexus::AddonFlags::None,
    provider: nexus::UpdateProvider::None,
}

fn load() {
    game::initialize();

    add_quick_access(
        QA_ID,
        "",
        "",
        TOGGLE_KEY,
        "Toggle Legendary Manager",
    )
    .revert_on_unload();

    nexus::keybind::register_keybind_with_string(
        TOGGLE_KEY,
        nexus::keybind::keybind_handler!(|_id, is_release| {
            if !is_release {
                let mut s = STATE.lock();
                s.visible = !s.visible;
            }
        }),
        "",
    )
    .revert_on_unload();

    register_render(
        RenderType::Render,
        render!(|ui| {
            tick_and_render(ui);
        }),
    )
    .revert_on_unload();
}

fn unload() {
    remove_quick_access(QA_ID);
}

fn tick_and_render(ui: &Ui) {
    let playable = game::is_playable();
    if !playable {
        LAST_PLAYABLE.store(false, Ordering::Relaxed);
        let mut s = STATE.lock();
        s.last_status = if game::init_error().is_empty() {
            "Game not in playable state".to_string()
        } else {
            format!("Not playable: {}", game::init_error())
        };
        if s.visible {
            render_window(ui, &mut s, false);
        }
        return;
    }

    if !LAST_PLAYABLE.swap(true, Ordering::Relaxed) {
        STATE.lock().last_status = "Playable state detected".to_string();
    }

    let mut s = STATE.lock();
    if s.visible {
        render_window(ui, &mut s, true);
    }
}

fn render_window(ui: &Ui, s: &mut AppState, playable: bool) {
    let mut open = s.visible;
    Window::new("Legendary Manager")
        .size([760.0, 520.0], Condition::FirstUseEver)
        .flags(WindowFlags::NO_COLLAPSE)
        .opened(&mut open)
        .build(ui, || {
            ui.text("Legendary Equipment Manager (direct live apply)");
            ui.separator();
            ui.text(format!("Playable: {}", playable));
            ui.text(format!("Status: {}", s.last_status));

            if !playable {
                ui.separator();
                ui.text_wrapped(
                    "Addon logic is disabled while not in gameplay.",
                );
                return;
            }

            ui.separator();
            if ui.button("Refresh Live Equipment") {
                match game::read_live_equipment() {
                    Ok(slots) => {
                        s.live_slots = slots;
                        s.last_status = format!("Read {} live equipped entries", s.live_slots.len());
                    }
                    Err(e) => s.last_status = format!("Refresh failed: {e}"),
                }
            }

            ui.same_line();
            if ui.button("Capture Preset From Live") {
                if s.live_slots.is_empty() {
                    s.last_status = "No live equipment loaded; refresh first".to_string();
                } else {
                    let n = s.presets.len() + 1;
                    let preset = LegendaryPreset {
                        name: format!("Preset {}", n),
                        choices: s
                            .live_slots
                            .iter()
                            .map(|e| SlotChoice {
                                slot_index: e.slot_index,
                                item_id: e.item_id,
                                item_def_ptr: e.item_def_ptr as u64,
                            })
                            .collect(),
                    };
                    s.presets.push(preset);
                    s.last_status = format!("Captured Preset {}", n);
                }
            }

            ui.separator();
            ui.text("Live Equipped Entries");
            for e in &s.live_slots {
                ui.bullet_text(format!(
                    "slot={} item_id={} item_def=0x{:X}",
                    e.slot_index, e.item_id, e.item_def_ptr
                ));
            }

            ui.separator();
            ui.text("Presets");
            for (idx, preset) in s.presets.iter().enumerate() {
                ui.bullet_text(format!("{} ({} entries)", preset.name, preset.choices.len()));
                ui.same_line();
                let label = format!("Apply Direct##{}", idx);
                if ui.small_button(&label) {
                    let mut ok = 0usize;
                    let mut fail = 0usize;
                    let mut last_err = String::new();
                    for ch in &preset.choices {
                        match game::apply_itemdef_to_slot(ch.item_def_ptr as usize, ch.slot_index) {
                            Ok(()) => ok += 1,
                            Err(e) => {
                                fail += 1;
                                last_err = e;
                            }
                        }
                    }
                    s.last_status = if fail == 0 {
                        format!("Applied '{}' directly: {} OK", preset.name, ok)
                    } else {
                        format!(
                            "Applied '{}' with failures: {} OK / {} failed (last: {})",
                            preset.name, ok, fail, last_err
                        )
                    };
                }
            }

            ui.separator();
            ui.text_wrapped(
                "Direct mode: applies by calling inventory vtable slot +0x558 with stored ItemDef* and equipment slot index, skipping UI frame invocation.",
            );
        });

    s.visible = open;
}
