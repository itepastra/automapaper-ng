use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub(crate) enum ColorValue {
    ColorRgb([f32; 3]),
    ColorRgba([f32; 4]),
}

impl Into<[f32; 4]> for ColorValue {
    fn into(self) -> [f32; 4] {
        match self {
            ColorValue::ColorRgb([r, g, b]) => [r, g, b, 1.0],
            ColorValue::ColorRgba([r, g, b, a]) => [r, g, b, a],
        }
    }
}

pub fn parse_uniform_value(s: &str) -> Result<ColorValue, Box<dyn std::error::Error>> {
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    return parse_hex_color(s);
}

fn parse_hex_color(hex: &str) -> Result<ColorValue, Box<dyn std::error::Error>> {
    fn byte(p: &str) -> Result<u8, Box<dyn std::error::Error>> {
        Ok(u8::from_str_radix(p, 16)?)
    }

    match hex.len() {
        6 => {
            let r = byte(&hex[0..2])? as f32 / 255.0;
            let g = byte(&hex[2..4])? as f32 / 255.0;
            let b = byte(&hex[4..6])? as f32 / 255.0;
            Ok(ColorValue::ColorRgb([r, g, b]))
        }
        8 => {
            let r = byte(&hex[0..2])? as f32 / 255.0;
            let g = byte(&hex[2..4])? as f32 / 255.0;
            let b = byte(&hex[4..6])? as f32 / 255.0;
            let a = byte(&hex[6..8])? as f32 / 255.0;
            Ok(ColorValue::ColorRgba([r, g, b, a]))
        }
        _ => Err("expected #RRGGBB or #RRGGBBAA".into()),
    }
}
