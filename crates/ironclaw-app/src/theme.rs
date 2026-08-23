use gpui::{rgb, Rgba};

pub fn bg() -> Rgba {
    rgb(0x111318)
}
pub fn rail() -> Rgba {
    rgb(0x090b0f)
}
pub fn sidebar() -> Rgba {
    rgb(0x181b22)
}
pub fn panel() -> Rgba {
    rgb(0x1e222b)
}
pub fn panel_2() -> Rgba {
    rgb(0x242934)
}
pub fn text() -> Rgba {
    rgb(0xf4f6fb)
}
pub fn muted() -> Rgba {
    rgb(0x9199aa)
}
pub fn border() -> Rgba {
    rgb(0x303642)
}
pub fn accent() -> Rgba {
    rgb(0x8b7cf6)
}
pub fn accent_2() -> Rgba {
    rgb(0x55d6be)
}
pub fn danger() -> Rgba {
    rgb(0xf26b76)
}
pub fn warning() -> Rgba {
    rgb(0xe9b44c)
}
pub fn user_bubble() -> Rgba {
    rgb(0x5f52bd)
}
pub fn transparent() -> Rgba {
    Rgba {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 0.,
    }
}

pub fn avatar_color(name: &str) -> Rgba {
    match avatar_color_index(name) {
        0 => rgb(0x6b64c9),
        1 => rgb(0x2f7f79),
        2 => rgb(0x4d6f94),
        3 => rgb(0x7a6148),
        4 => rgb(0x5f754a),
        5 => rgb(0x8a5a72),
        _ => rgb(0x3d6d8a),
    }
}

pub fn avatar_color_index(name: &str) -> usize {
    const COLOR_COUNT: usize = 7;
    let mut hash: u32 = 0;
    for byte in name.bytes() {
        hash = hash.wrapping_mul(3).wrapping_add(byte as u32);
    }
    (hash as usize) % COLOR_COUNT
}

pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engineering_team_avatars_are_distinct() {
        let names = ["Maya", "Ravi", "Nora", "Leo", "Zoe"];
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            assert_eq!(initials(name).len(), 1);
            seen.insert(avatar_color_index(name));
        }
        assert_eq!(seen.len(), names.len());
    }
}
