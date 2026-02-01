use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Deserialize;

const REMOTE_URL: &str = "https://qjv.dev.br/armory/data/items_filtered.json";

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
pub struct Item {
    pub id: u32,
    pub name: String,
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(default)]
    pub rarity: String,
    #[serde(default)]
    pub level: u32,
    #[serde(default)]
    pub details: Option<ItemDetails>,
}

#[derive(Deserialize, Clone)]
pub struct ItemDetails {
    #[serde(rename = "type", default)]
    pub detail_type: Option<String>,
}

impl Item {
    pub fn is_upgrade(&self) -> bool {
        self.item_type == "UpgradeComponent"
    }

    pub fn upgrade_subtype(&self) -> &str {
        self.details
            .as_ref()
            .and_then(|d| d.detail_type.as_deref())
            .unwrap_or("Unknown")
    }

    pub fn display_label(&self) -> String {
        format!("[{}] {} ({} {})", self.id, self.name, self.rarity, self.item_type)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ItemFilter {
    All,
    UpgradeComponent,
    Weapon,
    Armor,
    Trinket,
    Back,
    Consumable,
    Other,
}

impl ItemFilter {
    pub const ALL: &[ItemFilter] = &[
        ItemFilter::All,
        ItemFilter::UpgradeComponent,
        ItemFilter::Weapon,
        ItemFilter::Armor,
        ItemFilter::Trinket,
        ItemFilter::Back,
        ItemFilter::Consumable,
        ItemFilter::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ItemFilter::All => "All",
            ItemFilter::UpgradeComponent => "Upgrade Component",
            ItemFilter::Weapon => "Weapon",
            ItemFilter::Armor => "Armor",
            ItemFilter::Trinket => "Trinket",
            ItemFilter::Back => "Back",
            ItemFilter::Consumable => "Consumable",
            ItemFilter::Other => "Other",
        }
    }

    pub fn matches(self, item: &Item) -> bool {
        match self {
            ItemFilter::All => true,
            ItemFilter::UpgradeComponent => item.item_type == "UpgradeComponent",
            ItemFilter::Weapon => item.item_type == "Weapon",
            ItemFilter::Armor => item.item_type == "Armor",
            ItemFilter::Trinket => item.item_type == "Trinket",
            ItemFilter::Back => item.item_type == "Back",
            ItemFilter::Consumable => item.item_type == "Consumable",
            ItemFilter::Other => !matches!(
                item.item_type.as_str(),
                "UpgradeComponent" | "Weapon" | "Armor" | "Trinket" | "Back" | "Consumable"
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpgradeFilter {
    All,
    Rune,
    Sigil,
    Gem,
    Default,
}

impl UpgradeFilter {
    pub const ALL: &[UpgradeFilter] = &[
        UpgradeFilter::All,
        UpgradeFilter::Rune,
        UpgradeFilter::Sigil,
        UpgradeFilter::Gem,
        UpgradeFilter::Default,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UpgradeFilter::All => "All Upgrades",
            UpgradeFilter::Rune => "Rune",
            UpgradeFilter::Sigil => "Sigil",
            UpgradeFilter::Gem => "Gem",
            UpgradeFilter::Default => "Default (Infusion/Jewel)",
        }
    }

    pub fn matches(self, item: &Item) -> bool {
        if !item.is_upgrade() {
            return false;
        }
        match self {
            UpgradeFilter::All => true,
            UpgradeFilter::Rune => item.upgrade_subtype() == "Rune",
            UpgradeFilter::Sigil => item.upgrade_subtype() == "Sigil",
            UpgradeFilter::Gem => item.upgrade_subtype() == "Gem",
            UpgradeFilter::Default => item.upgrade_subtype() == "Default",
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum DbStatus {
    NotLoaded,
    Loading,
    Loaded,
    Error,
    Updating,
}

pub struct ItemDatabase {
    pub items: Vec<Item>,
    pub status: DbStatus,
    pub error_msg: String,
    pub last_id: u32,
}

impl Default for ItemDatabase {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            status: DbStatus::NotLoaded,
            error_msg: String::new(),
            last_id: 0,
        }
    }
}

pub static ITEM_DB: Lazy<Mutex<ItemDatabase>> = Lazy::new(|| Mutex::new(ItemDatabase::default()));

fn get_db_path() -> Option<std::path::PathBuf> {
    nexus::paths::get_addon_dir("chat_link_generator").map(|d| d.join("items_filtered.json"))
}

pub fn load_item_db() {
    {
        let mut db = ITEM_DB.lock();
        if db.status == DbStatus::Loading || db.status == DbStatus::Loaded {
            return;
        }
        db.status = DbStatus::Loading;
    }

    std::thread::spawn(move || {
        let result = load_from_disk_or_remote();
        let mut db = ITEM_DB.lock();
        match result {
            Ok(items) => {
                db.last_id = items.last().map(|i| i.id).unwrap_or(0);
                nexus::log::log(
                    nexus::log::LogLevel::Info,
                    "Chat Link Generator",
                    &format!("Loaded {} items (last ID: {})", items.len(), db.last_id),
                );
                db.items = items;
                db.status = DbStatus::Loaded;
            }
            Err(e) => {
                nexus::log::log(
                    nexus::log::LogLevel::Critical,
                    "Chat Link Generator",
                    &format!("Failed to load item DB: {}", e),
                );
                db.error_msg = e;
                db.status = DbStatus::Error;
            }
        }
    });
}

fn load_from_disk_or_remote() -> Result<Vec<Item>, String> {
    if let Some(path) = get_db_path() {
        if path.exists() {
            let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let items: Vec<Item> = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            if !items.is_empty() {
                return Ok(items);
            }
        }
    }

    // No local copy, fetch from remote
    fetch_and_save()
}

fn fetch_and_save() -> Result<Vec<Item>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    rt.block_on(async {
        let resp = reqwest::get(REMOTE_URL).await.map_err(|e| e.to_string())?;
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;

        // Save to disk
        if let Some(path) = get_db_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &bytes);
        }

        let items: Vec<Item> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(items)
    })
}

pub fn check_for_update() {
    {
        let mut db = ITEM_DB.lock();
        if db.status == DbStatus::Updating || db.status == DbStatus::Loading {
            return;
        }
        db.status = DbStatus::Updating;
    }

    std::thread::spawn(move || {
        let result = fetch_and_save();
        let mut db = ITEM_DB.lock();
        match result {
            Ok(items) => {
                let new_last = items.last().map(|i| i.id).unwrap_or(0);
                if new_last != db.last_id {
                    nexus::log::log(
                        nexus::log::LogLevel::Info,
                        "Chat Link Generator",
                        &format!("Item DB updated: {} -> {} (last ID)", db.last_id, new_last),
                    );
                    db.last_id = new_last;
                    db.items = items;
                } else {
                    nexus::log::log(
                        nexus::log::LogLevel::Info,
                        "Chat Link Generator",
                        "Item DB already up to date",
                    );
                }
                db.status = DbStatus::Loaded;
            }
            Err(e) => {
                nexus::log::log(
                    nexus::log::LogLevel::Critical,
                    "Chat Link Generator",
                    &format!("Failed to update item DB: {}", e),
                );
                db.error_msg = e;
                // Keep loaded status if we had data before
                if !db.items.is_empty() {
                    db.status = DbStatus::Loaded;
                } else {
                    db.status = DbStatus::Error;
                }
            }
        }
    });
}

pub fn search_items(query: &str, filter: ItemFilter, max_results: usize) -> Vec<(u32, String)> {
    let db = ITEM_DB.lock();
    if db.items.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    db.items
        .iter()
        .filter(|item| {
            filter.matches(item) && item.name.to_lowercase().contains(&query_lower)
        })
        .take(max_results)
        .map(|item| (item.id, item.display_label()))
        .collect()
}

pub fn search_upgrades(query: &str, upgrade_filter: UpgradeFilter, max_results: usize) -> Vec<(u32, String)> {
    let db = ITEM_DB.lock();
    if db.items.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    db.items
        .iter()
        .filter(|item| {
            upgrade_filter.matches(item) && item.name.to_lowercase().contains(&query_lower)
        })
        .take(max_results)
        .map(|item| (item.id, item.display_label()))
        .collect()
}
