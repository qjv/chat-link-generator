use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::encoder::LinkType;

#[derive(Serialize, Deserialize, Clone)]
pub struct UserConfig {
    #[serde(default = "default_link_type_index")]
    pub link_type_index: usize,
    #[serde(default = "default_batch_size")]
    pub batch_size: i32,
    #[serde(default)]
    pub show_id_prefix: bool,
}

fn default_link_type_index() -> usize { 0 }
fn default_batch_size() -> i32 { 50 }

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            link_type_index: 0,
            batch_size: 50,
            show_id_prefix: false,
        }
    }
}

pub struct RuntimeConfig {
    pub show_main_window: bool,
    pub link_type_index: usize,
    pub start_id: i32,
    pub batch_size: i32,
    pub show_id_prefix: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            show_main_window: false,
            link_type_index: 0,
            start_id: LinkType::ALL[0].default_start() as i32,
            batch_size: 50,
            show_id_prefix: false,
        }
    }
}

pub static RUNTIME_CONFIG: Lazy<Mutex<RuntimeConfig>> =
    Lazy::new(|| Mutex::new(RuntimeConfig::default()));

pub static USER_CONFIG: Lazy<Mutex<UserConfig>> =
    Lazy::new(|| Mutex::new(UserConfig::default()));

pub fn load_user_config() {
    let Some(dir) = nexus::paths::get_addon_dir("chat_link_generator") else {
        return;
    };
    let path = dir.join("user_config.json");
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<UserConfig>(&data) {
            let mut rt = RUNTIME_CONFIG.lock();
            rt.link_type_index = cfg.link_type_index.min(LinkType::ALL.len() - 1);
            rt.batch_size = cfg.batch_size;
            rt.show_id_prefix = cfg.show_id_prefix;
            rt.start_id = LinkType::ALL[rt.link_type_index].default_start() as i32;
            *USER_CONFIG.lock() = cfg;
        }
    }
}

pub fn save_user_config() {
    let Some(dir) = nexus::paths::get_addon_dir("chat_link_generator") else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("user_config.json");

    let cfg = {
        let rt = RUNTIME_CONFIG.lock();
        UserConfig {
            link_type_index: rt.link_type_index,
            batch_size: rt.batch_size,
            show_id_prefix: rt.show_id_prefix,
        }
    };

    if let Ok(json) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(&path, json);
    }
}
