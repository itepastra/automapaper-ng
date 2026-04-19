use serde::{Deserialize, Serialize};

impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_uniform_value(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ColorValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match *self {
            ColorValue::ColorRgb([r, g, b]) => serializer.collect_str(&format!(
                "{:02x}{:02x}{:02x}",
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (b.clamp(0.0, 1.0) * 255.0).round() as u8,
            )),
            ColorValue::ColorRgba([r, g, b, a]) => serializer.collect_str(&format!(
                "{:02x}{:02x}{:02x}{:02x}",
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ColorValue {
    ColorRgb([f32; 3]),
    ColorRgba([f32; 4]),
}

impl From<ColorValue> for [f32; 4] {
    fn from(val: ColorValue) -> Self {
        match val {
            ColorValue::ColorRgb([r, g, b]) => [r, g, b, 1.0],
            ColorValue::ColorRgba([r, g, b, a]) => [r, g, b, a],
        }
    }
}

pub fn parse_uniform_value(s: &str) -> Result<ColorValue, Box<dyn std::error::Error>> {
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    parse_hex_color(s)
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
