const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkType {
    Item,
    Map,
    Skill,
    Trait,
    Recipe,
    Wardrobe,
    Outfit,
}

impl LinkType {
    pub const ALL: &[LinkType] = &[
        LinkType::Item,
        LinkType::Map,
        LinkType::Skill,
        LinkType::Trait,
        LinkType::Recipe,
        LinkType::Wardrobe,
        LinkType::Outfit,
    ];

    pub fn header_byte(self) -> u8 {
        match self {
            LinkType::Item => 0x02,
            LinkType::Map => 0x04,
            LinkType::Skill => 0x06,
            LinkType::Trait => 0x07,
            LinkType::Recipe => 0x09,
            LinkType::Wardrobe => 0x0A,
            LinkType::Outfit => 0x0B,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LinkType::Item => "Item",
            LinkType::Map => "Map (Waypoint/POI)",
            LinkType::Skill => "Skill",
            LinkType::Trait => "Trait",
            LinkType::Recipe => "Recipe",
            LinkType::Wardrobe => "Wardrobe Skin",
            LinkType::Outfit => "Outfit",
        }
    }

    pub fn default_start(self) -> u32 {
        match self {
            LinkType::Item => 106898,
            LinkType::Map => 4056,
            LinkType::Skill => 76376,
            LinkType::Trait => 2443,
            LinkType::Recipe => 15441,
            LinkType::Wardrobe => 13684,
            LinkType::Outfit => 138,
        }
    }
}

fn encode_id_le3(id: u32) -> [u8; 3] {
    [
        (id & 0xFF) as u8,
        ((id >> 8) & 0xFF) as u8,
        ((id >> 16) & 0xFF) as u8,
    ]
}

/// Generate a simple chat link (non-item types).
/// Format: [header][id LE 3 bytes][0x00]
pub fn generate_simple_link(link_type: LinkType, id: u32) -> String {
    let id_bytes = encode_id_le3(id);
    let bytes = vec![link_type.header_byte(), id_bytes[0], id_bytes[1], id_bytes[2], 0x00];
    format_chat_link(&bytes)
}

/// Generate an item chat link with all optional fields.
/// Format: [0x02][quantity][id LE 3][flags][skin LE 3?][upgrade1 LE 3?][upgrade2 LE 3?][0x00]
pub fn generate_item_link(
    id: u32,
    quantity: u8,
    skin_id: Option<u32>,
    first_upgrade_id: Option<u32>,
    second_upgrade_id: Option<u32>,
) -> String {
    let id_bytes = encode_id_le3(id);

    let mut flags: u8 = 0;
    if skin_id.is_some() {
        flags |= 0x80;
    }
    if first_upgrade_id.is_some() {
        flags |= 0x40;
    }
    if second_upgrade_id.is_some() {
        flags |= 0x20;
    }

    let mut bytes = vec![
        0x02,
        quantity,
        id_bytes[0], id_bytes[1], id_bytes[2],
        flags,
    ];

    if let Some(skin) = skin_id {
        let s = encode_id_le3(skin);
        bytes.extend_from_slice(&s);
    }
    if let Some(up1) = first_upgrade_id {
        let u = encode_id_le3(up1);
        bytes.extend_from_slice(&u);
    }
    if let Some(up2) = second_upgrade_id {
        let u = encode_id_le3(up2);
        bytes.extend_from_slice(&u);
    }

    bytes.push(0x00);
    format_chat_link(&bytes)
}

/// Generate a batch link for the given type and id.
/// For items, generates a simple link with quantity=1 and no extras.
pub fn generate_batch_link(link_type: LinkType, id: u32) -> String {
    match link_type {
        LinkType::Item => {
            let id_bytes = encode_id_le3(id);
            let bytes = vec![0x02, 1, id_bytes[0], id_bytes[1], id_bytes[2], 0x00];
            format_chat_link(&bytes)
        }
        _ => generate_simple_link(link_type, id),
    }
}

fn format_chat_link(bytes: &[u8]) -> String {
    let encoded = base64_encode(bytes);
    format!("[&{}]", encoded)
}
