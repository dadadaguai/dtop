use ratatui::style::Color;

pub struct Theme {
    pub name: &'static str,
    pub border: Color,
    pub title: Color,
    pub text: Color,
    pub dim: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub cyan: Color,
    #[allow(dead_code)]
    pub orange: Color,
    pub magenta: Color,
    #[allow(dead_code)]
    pub sel: Color,
}

fn rgb(hex: u32) -> Color {
    Color::Rgb(((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8)
}

pub fn themes() -> Vec<Theme> {
    vec![
        Theme {
            name: "dark",
            border: rgb(0x2b3945),
            title: rgb(0x7fd4ff),
            text: rgb(0xc7d3dd),
            dim: rgb(0x6b7c8a),
            green: rgb(0x35e08a),
            yellow: rgb(0xffc94d),
            red: rgb(0xff5d5d),
            cyan: rgb(0x38d9f5),
            orange: rgb(0xff9f43),
            magenta: rgb(0xd16bd1),
            sel: rgb(0x1f4b66),
        },
        Theme {
            name: "nord",
            border: rgb(0x434c5e),
            title: rgb(0x88c0d0),
            text: rgb(0xd8dee9),
            dim: rgb(0x7b88a1),
            green: rgb(0xa3be8c),
            yellow: rgb(0xebcb8b),
            red: rgb(0xbf616a),
            cyan: rgb(0x8fbcbb),
            orange: rgb(0xd08770),
            magenta: rgb(0xb48ead),
            sel: rgb(0x434c5e),
        },
        Theme {
            name: "light",
            border: rgb(0xb9b4a8),
            title: rgb(0x1a6e8f),
            text: rgb(0x33322e),
            dim: rgb(0x85817a),
            green: rgb(0x1f8a4c),
            yellow: rgb(0xb57f00),
            red: rgb(0xc22f2f),
            cyan: rgb(0x0e7ea3),
            orange: rgb(0xb35f1e),
            magenta: rgb(0x8a3d8a),
            sel: rgb(0xcfe3ec),
        },
    ]
}

pub fn theme_index(name: &str) -> usize {
    themes().iter().position(|t| t.name == name).unwrap_or(0)
}

