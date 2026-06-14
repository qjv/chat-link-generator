use nexus::data_link::read_nexus_link;
use nexus::gui::{register_render, render, RenderType};
use nexus::imgui::{CollapsingHeader, Condition, InputText, Selectable, Ui, Window};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

const SAVE_FILE: &str = "presets.json";

const SLOTS: &[&str] = &[
    "helm",
    "shoulders",
    "coat",
    "gloves",
    "leggings",
    "boots",
    "mainhand_a",
    "offhand_a",
    "twohand_a",
    "mainhand_b",
    "offhand_b",
    "twohand_b",
    "amulet",
    "ring_1",
    "ring_2",
    "accessory_1",
    "accessory_2",
    "back",
    "relic",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EquipmentEntry {
    slot: String,
    item_name: String,
    stat_set: String,
    upgrade_1: String,
    upgrade_2: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Preset {
    name: String,
    entries: Vec<EquipmentEntry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SaveData {
    presets: Vec<Preset>,
}

#[derive(Debug)]
struct AppState {
    show_window: bool,
    selected_preset: Option<usize>,
    preset_name_input: String,
    editable: BTreeMap<String, EquipmentEntry>,
    active_loadout: BTreeMap<String, EquipmentEntry>,
    save: SaveData,
    status: String,
}

impl Default for AppState {
    fn default() -> Self {
        let mut editable = BTreeMap::new();
        let mut active_loadout = BTreeMap::new();
        for slot in SLOTS {
            let entry = EquipmentEntry {
                slot: (*slot).to_string(),
                item_name: "(empty)".to_string(),
                stat_set: "(none)".to_string(),
                upgrade_1: "(none)".to_string(),
                upgrade_2: "(none)".to_string(),
            };
            editable.insert((*slot).to_string(), entry.clone());
            active_loadout.insert((*slot).to_string(), entry);
        }

        Self {
            show_window: true,
            selected_preset: None,
            preset_name_input: String::new(),
            editable,
            active_loadout,
            save: load_save_data().unwrap_or_default(),
            status: "Ready".to_string(),
        }
    }
}

static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));

nexus::export! {
    name: "Legendary Preset Manager",
    signature: -0x4C504D47,
    load,
    unload,
    flags: nexus::AddonFlags::None,
}

fn load() {
    register_render(
        RenderType::Render,
        render!(|ui| {
            render_main(ui);
        }),
    )
    .revert_on_unload();
}

fn unload() {
    let state = STATE.lock();
    let _ = save_data(&state.save);
}

fn render_main(ui: &Ui) {
    let is_gameplay = read_nexus_link().is_some_and(|n| n.is_gameplay);

    let mut state = STATE.lock();
    if !state.show_window {
        return;
    }

    Window::new("Legendary Presets")
        .size([840.0, 620.0], Condition::FirstUseEver)
        .build(ui, || {
            if !is_gameplay {
                ui.text_colored(
                    [1.0, 0.65, 0.2, 1.0],
                    "Addon locked: only available while gameplay is active.",
                );
                return;
            }

            ui.text("Available Equipment (editable draft)");
            ui.separator();

            for slot in SLOTS {
                if let Some(entry) = state.editable.get_mut(*slot) {
                    ui.text(slot);
                    ui.same_line();
                    let item_id = format!("##item_{slot}");
                    InputText::new(ui, &item_id, &mut entry.item_name).build();
                    ui.same_line();
                    let stat_id = format!("##stat_{slot}");
                    InputText::new(ui, &stat_id, &mut entry.stat_set).build();
                    ui.same_line();
                    let up1_id = format!("##up1_{slot}");
                    InputText::new(ui, &up1_id, &mut entry.upgrade_1).build();
                    ui.same_line();
                    let up2_id = format!("##up2_{slot}");
                    InputText::new(ui, &up2_id, &mut entry.upgrade_2).build();
                }
            }

            ui.separator();
            ui.text("Preset Name");
            InputText::new(ui, "##preset_name", &mut state.preset_name_input).build();
            ui.same_line();
            if ui.button("Create Preset") {
                if state.preset_name_input.trim().is_empty() {
                    state.status = "Preset name cannot be empty".to_string();
                } else {
                    let name = state.preset_name_input.trim().to_string();
                    let entries = state.editable.values().cloned().collect::<Vec<_>>();
                    state.save.presets.push(Preset { name, entries });
                    state.preset_name_input.clear();
                    state.status = "Preset created".to_string();
                    let _ = save_data(&state.save);
                }
            }

            if CollapsingHeader::new("Saved Presets").default_open(true).build(ui) {
                for (idx, preset) in state.save.presets.iter().enumerate() {
                    let selected = state.selected_preset == Some(idx);
                    if Selectable::new(&preset.name).selected(selected).build(ui) {
                        state.selected_preset = Some(idx);
                    }
                }
            }

            ui.separator();
            if ui.button("Apply Selected Preset (One Click)") {
                if let Some(idx) = state.selected_preset {
                    if let Some(preset) = state.save.presets.get(idx) {
                        state.active_loadout.clear();
                        for entry in &preset.entries {
                            state
                                .active_loadout
                                .insert(entry.slot.clone(), entry.clone());
                        }
                        state.status = format!("Applied preset '{}'", preset.name);
                    }
                } else {
                    state.status = "No preset selected".to_string();
                }
            }

            ui.separator();
            ui.text("Active Loadout");
            for slot in SLOTS {
                if let Some(entry) = state.active_loadout.get(*slot) {
                    ui.bullet_text(format!(
                        "{}: {} | {} | {} | {}",
                        entry.slot,
                        entry.item_name,
                        entry.stat_set,
                        entry.upgrade_1,
                        entry.upgrade_2
                    ));
                }
            }

            ui.separator();
            ui.text(format!("Status: {}", state.status));
        });
}

fn save_path() -> Option<PathBuf> {
    nexus::paths::get_addon_dir("legendary-preset-addon").map(|d| d.join(SAVE_FILE))
}

fn load_save_data() -> Option<SaveData> {
    let path = save_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn save_data(data: &SaveData) -> std::io::Result<()> {
    let Some(path) = save_path() else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(data)
        .map_err(|err| std::io::Error::other(format!("serialize presets: {err}")))?;
    std::fs::write(path, json)
}
