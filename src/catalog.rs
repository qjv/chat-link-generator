use std::collections::BTreeMap;

use crate::db::{self, DbStatus};
use crate::encoder::{self, LinkType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceView {
    Merged,
    Api,
    Game,
    Differences,
    MissingApi,
    MissingGame,
}

impl SourceView {
    pub const ALL: &[SourceView] = &[
        SourceView::Merged,
        SourceView::Api,
        SourceView::Game,
        SourceView::Differences,
        SourceView::MissingApi,
        SourceView::MissingGame,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SourceView::Merged => "Merged",
            SourceView::Api => "API",
            SourceView::Game => "Game Memory",
            SourceView::Differences => "Differences",
            SourceView::MissingApi => "Game Only",
            SourceView::MissingGame => "API Only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogSort {
    IdAsc,
    IdDesc,
    NameAsc,
    NameDesc,
    Coverage,
}

impl CatalogSort {
    pub const ALL: &[CatalogSort] = &[
        CatalogSort::IdAsc,
        CatalogSort::IdDesc,
        CatalogSort::NameAsc,
        CatalogSort::NameDesc,
        CatalogSort::Coverage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CatalogSort::IdAsc => "ID Asc",
            CatalogSort::IdDesc => "ID Desc",
            CatalogSort::NameAsc => "Name A-Z",
            CatalogSort::NameDesc => "Name Z-A",
            CatalogSort::Coverage => "Coverage",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComparisonState {
    Same,
    Different,
    ApiOnly,
    GameOnly,
    Unknown,
}

impl ComparisonState {
    pub fn label(self) -> &'static str {
        match self {
            ComparisonState::Same => "Same",
            ComparisonState::Different => "Different",
            ComparisonState::ApiOnly => "API Only",
            ComparisonState::GameOnly => "Game Only",
            ComparisonState::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SourceStatus {
    pub api: (DbStatus, usize, String, Option<(usize, usize)>),
    pub game: (DbStatus, usize, String, Option<(usize, usize)>),
}

#[derive(Clone, Debug)]
pub struct CatalogRecord {
    pub link_type: LinkType,
    pub id: u32,
    pub api_name: Option<String>,
    pub game_name: Option<String>,
    pub comparison: ComparisonState,
    pub chat_link: String,
}

impl CatalogRecord {
    pub fn display_name(&self) -> &str {
        self.game_name
            .as_deref()
            .or(self.api_name.as_deref())
            .unwrap_or("")
    }

    pub fn has_api(&self) -> bool {
        self.api_name.is_some()
    }

    pub fn has_game(&self) -> bool {
        self.game_name.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct CatalogQuery {
    pub link_type: LinkType,
    pub source_view: SourceView,
    pub search: String,
    pub min_id: u32,
    pub max_id: Option<u32>,
    pub sort: CatalogSort,
}

pub fn ensure_sources_loaded(link_type: LinkType) {
    db::ensure_loaded(link_type);
    db::ingame_items::ensure_loaded();
}

pub fn source_status(link_type: LinkType) -> SourceStatus {
    let api = db::get_status(link_type);
    let mut game = db::ingame_items::get_status();
    if link_type != LinkType::Item {
        let count = db::ingame_items::get_game_data_for_link_type(link_type, "", usize::MAX).len();
        game.1 = count;
    }
    SourceStatus { api, game }
}

pub fn query_records(query: &CatalogQuery) -> Vec<CatalogRecord> {
    let api_rows = if matches!(query.source_view, SourceView::Game | SourceView::MissingApi) {
        Vec::new()
    } else {
        db::search(query.link_type, "", 0, usize::MAX)
    };
    let game_rows = if matches!(query.source_view, SourceView::Api | SourceView::MissingGame) {
        Vec::new()
    } else {
        db::ingame_items::get_game_data_for_link_type(query.link_type, "", usize::MAX)
    };

    let mut merged: BTreeMap<u32, (Option<String>, Option<String>)> = BTreeMap::new();
    for (id, name) in api_rows {
        merged.entry(id).or_default().0 = Some(name);
    }
    for row in game_rows {
        merged.entry(row.id).or_default().1 = Some(row.name);
    }

    let search = query.search.trim().to_lowercase();
    let mut rows: Vec<CatalogRecord> = merged
        .into_iter()
        .filter_map(|(id, (api_name, game_name))| {
            if id < query.min_id {
                return None;
            }
            if let Some(max_id) = query.max_id {
                if id > max_id {
                    return None;
                }
            }

            let comparison = compare_names(api_name.as_deref(), game_name.as_deref());
            let keep = match query.source_view {
                SourceView::Merged => true,
                SourceView::Api => api_name.is_some(),
                SourceView::Game => game_name.is_some(),
                SourceView::Differences => comparison == ComparisonState::Different,
                SourceView::MissingApi => api_name.is_none() && game_name.is_some(),
                SourceView::MissingGame => api_name.is_some() && game_name.is_none(),
            };
            if !keep {
                return None;
            }

            if !search.is_empty() {
                let haystack = format!(
                    "{} {} {}",
                    id,
                    api_name.as_deref().unwrap_or(""),
                    game_name.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if !haystack.contains(&search) {
                    return None;
                }
            }

            Some(CatalogRecord {
                link_type: query.link_type,
                id,
                api_name,
                game_name,
                comparison,
                chat_link: encoder::generate_batch_link(query.link_type, id),
            })
        })
        .collect();

    match query.sort {
        CatalogSort::IdAsc => rows.sort_by_key(|r| r.id),
        CatalogSort::IdDesc => rows.sort_by(|a, b| b.id.cmp(&a.id)),
        CatalogSort::NameAsc => rows.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
                .then(a.id.cmp(&b.id))
        }),
        CatalogSort::NameDesc => rows.sort_by(|a, b| {
            b.display_name()
                .to_lowercase()
                .cmp(&a.display_name().to_lowercase())
                .then(a.id.cmp(&b.id))
        }),
        CatalogSort::Coverage => rows.sort_by_key(|r| (r.comparison, r.id)),
    }

    rows
}

fn compare_names(api_name: Option<&str>, game_name: Option<&str>) -> ComparisonState {
    match (api_name, game_name) {
        (Some(api), Some(game)) => {
            let api = normalize_name(api);
            let game = normalize_name(game);
            if api.is_empty() || game.is_empty() {
                ComparisonState::Unknown
            } else if api.eq_ignore_ascii_case(&game) {
                ComparisonState::Same
            } else {
                ComparisonState::Different
            }
        }
        (Some(_), None) => ComparisonState::ApiOnly,
        (None, Some(_)) => ComparisonState::GameOnly,
        (None, None) => ComparisonState::Unknown,
    }
}

fn normalize_name(name: &str) -> String {
    let mut s = name.trim().to_string();
    if s.is_empty() || s == "-" {
        return String::new();
    }

    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            s = rest[end + 1..].trim_start().to_string();
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
