use std::{
    fs,
    path::{self, PathBuf},
    sync::mpsc,
};

use crate::{
    cli::{Cli, Command},
    config::{get_config, Config},
    ipc::{send_request_and_print, socket_path, spawn_ipc_server, IpcRequest},
    renderer::run_renderer,
    uniform::parse_uniform_value,
};
use clap::Parser;

mod cli;
mod config;
mod ipc;
mod renderer;
mod uniform;

#[derive(Debug)]
enum AppCommand {
    Set {
        name: String,
        value: uniform::UniformValue,
    },
    Shader {
        fragment_glsl: String,
    },
}

#[derive(Debug, Clone)]
struct UniformState {
    time_scale: f32,
    c1: [f32; 4],
    c2: [f32; 4],
    c3: [f32; 4],
    c4: [f32; 4],
}

impl Default for UniformState {
    fn default() -> Self {
        Self {
            time_scale: 1.0,
            c1: [1.0, 0.0, 0.0, 1.0],
            c2: [0.0, 0.0, 1.0, 1.0],
            c3: [0.0, 0.0, 1.0, 1.0],
            c4: [0.0, 0.0, 1.0, 1.0],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    resolution: [f32; 2],
    time: f32,
    time_scale: f32,
    c1: [f32; 4],
    c2: [f32; 4],
    c3: [f32; 4],
    c4: [f32; 4],
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();
    let config = get_config();

    match cli.command {
        None => {
            let socket_path = socket_path();
            let (tx, rx) = mpsc::channel::<AppCommand>();

            spawn_ipc_server(&socket_path, tx).unwrap_or_else(|e| {
                eprintln!("failed to start IPC server: {e}");
                std::process::exit(1);
            });

            run_renderer(rx, config);
        }
        Some(Command::Set { name, value }) => {
            let value = parse_uniform_value(&value).unwrap_or_else(|e| {
                eprintln!("failed to parse value: {e}");
                std::process::exit(2);
            });

            let req = IpcRequest::Set { name, value };
            send_request_and_print(&req).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        }
        Some(Command::DisplayShader { path }) => send_or_error(&path, ShaderType::Display),
        Some(Command::InitShader { path }) => send_or_error(&path, ShaderType::Init),
        Some(Command::StateShader { path }) => send_or_error(&path, ShaderType::State),
        Some(Command::Get { name }) => {
            let req = IpcRequest::Get { name };
            send_request_and_print(&req).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        }
    }
}

enum ShaderType {
    Display,
    Init,
    State,
}

fn send_or_error(path: &PathBuf, shader_type: ShaderType) {
    let fragment_glsl = fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("failed to read shader '{}': {e}", path.display());
        std::process::exit(2);
    });

    let req = match shader_type {
        ShaderType::Display => IpcRequest::DisplayShader { fragment_glsl },
        ShaderType::Init => IpcRequest::InitShader { fragment_glsl },
        ShaderType::State => IpcRequest::StateShader { fragment_glsl },
    };

    send_request_and_print(&req).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
}
