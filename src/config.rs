use std::{
    fs::{create_dir_all, read_to_string, File},
    io::Write,
    path::PathBuf,
    process,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::uniform::ColorValue;

#[derive(Serialize, Deserialize)]
pub struct Config {
    state_shader_path: PathBuf,
    display_shader_path: PathBuf,

    pub c1: ColorValue,
    pub c2: ColorValue,
    pub c3: ColorValue,
    pub c4: ColorValue,

    pub state_shrink_h: u32,
    pub state_shrink_v: u32,

    pub decay_time: f32,
    pub frame_time: f32,
}

fn get_config_dir() -> PathBuf {
    let mut config_file = dirs::config_dir().unwrap();
    config_file.push("automapaper-ng");
    return config_file;
}

fn true_path(path: &PathBuf) -> PathBuf {
    let abs_path = if path.is_relative() {
        &get_config_dir().join(path)
    } else {
        path
    };

    if !abs_path.exists() {
        println!("path {abs_path:?} does not exist");
        process::exit(3);
    }

    let can_path = abs_path.canonicalize().unwrap();
    return can_path;
}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_shader_path: PathBuf::from("./state.glsl"),
            display_shader_path: PathBuf::from("./display.glsl"),

            c1: ColorValue::ColorRgb([0., 0., 0.]),
            c2: ColorValue::ColorRgb([0., 1., 0.]),
            c3: ColorValue::ColorRgb([0., 0., 1.]),
            c4: ColorValue::ColorRgb([1., 1., 1.]),

            state_shrink_h: 10,
            state_shrink_v: 10,

            decay_time: 0.1,
            frame_time: 1.0 / 10.0,
        }
    }
}

impl Config {
    pub fn get_state_path(&self) -> PathBuf {
        true_path(&self.state_shader_path)
    }
    pub fn get_display_path(&self) -> PathBuf {
        true_path(&self.display_shader_path)
    }
}

pub fn get_config() -> Config {
    let mut config_file = get_config_dir();
    config_file.push("config.toml");

    if !config_file.exists() {
        println!("The file {config_file:?} does not exist");
        // initialise the default config file
        create_dir_all(config_file.parent().unwrap()).unwrap();
        let mut f = File::create(config_file).unwrap();
        f.write_all(
            toml::to_string_pretty(&Config::default())
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        return Config::default();
    }
    println!("The file {config_file:?} does exist");

    return toml::from_str(&read_to_string(config_file).unwrap()).unwrap();
}
