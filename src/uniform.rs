use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub(crate) enum UniformValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    ColorRgb([f32; 3]),
    ColorRgba([f32; 4]),
}

impl Into<[f32; 4]> for UniformValue {
    fn into(self) -> [f32; 4] {
        match self {
            UniformValue::Float(x) => [x, 0.0, 0.0, 1.0],
            UniformValue::Vec2([x, y]) => [x, y, 0.0, 0.0],
            UniformValue::Vec3([x, y, z]) => [x, y, z, 0.0],
            UniformValue::Vec4([x, y, z, a]) => [x, y, z, a],
            UniformValue::ColorRgb([r, g, b]) => [r, g, b, 1.0],
            UniformValue::ColorRgba([r, g, b, a]) => [r, g, b, a],
        }
    }
}

pub fn parse_uniform_value(s: &str) -> Result<UniformValue, Box<dyn std::error::Error>> {
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }

    if s.contains(',') {
        let parts: Vec<f32> = s
            .split(',')
            .map(|p| p.trim().parse::<f32>())
            .collect::<Result<_, _>>()?;

        return match parts.as_slice() {
            [x, y] => Ok(UniformValue::Vec2([*x, *y])),
            [x, y, z] => Ok(UniformValue::Vec3([*x, *y, *z])),
            [x, y, z, w] => Ok(UniformValue::Vec4([*x, *y, *z, *w])),
            _ => Err("expected 2, 3, or 4 comma-separated floats".into()),
        };
    }

    Ok(UniformValue::Float(s.parse()?))
}

fn parse_hex_color(hex: &str) -> Result<UniformValue, Box<dyn std::error::Error>> {
    fn byte(p: &str) -> Result<u8, Box<dyn std::error::Error>> {
        Ok(u8::from_str_radix(p, 16)?)
    }

    match hex.len() {
        6 => {
            let r = byte(&hex[0..2])? as f32 / 255.0;
            let g = byte(&hex[2..4])? as f32 / 255.0;
            let b = byte(&hex[4..6])? as f32 / 255.0;
            Ok(UniformValue::ColorRgb([r, g, b]))
        }
        8 => {
            let r = byte(&hex[0..2])? as f32 / 255.0;
            let g = byte(&hex[2..4])? as f32 / 255.0;
            let b = byte(&hex[4..6])? as f32 / 255.0;
            let a = byte(&hex[6..8])? as f32 / 255.0;
            Ok(UniformValue::ColorRgba([r, g, b, a]))
        }
        _ => Err("expected #RRGGBB or #RRGGBBAA".into()),
    }
}
