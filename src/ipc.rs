use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::mpsc,
};

use serde::{Deserialize, Serialize};

use crate::{uniform::UniformValue, AppCommand};

pub const SOCKET_NAME: &str = "automapaper-ng.sock";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum IpcRequest {
    Set { name: String, value: UniformValue },
    DisplayShader { fragment_glsl: String },
    StateShader { fragment_glsl: String },
    InitShader { fragment_glsl: String },
    Get { name: String },
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum IpcResponse {
    Ok,
    Value { name: String, value: String },
    Err { message: String },
    Pong,
}

pub fn spawn_ipc_server(
    socket_path: &Path,
    tx: mpsc::Sender<AppCommand>,
) -> Result<(), Box<dyn std::error::Error>> {
    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err("another instance is already running".into());
            }
            Err(_) => {
                let _ = fs::remove_file(socket_path);
            }
        }
    }

    let listener = UnixListener::bind(socket_path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };

            if let Err(err) = handle_client(stream, &tx) {
                eprintln!("ipc error: {err}");
            }
        }
    });

    Ok(())
}

fn handle_client(
    mut stream: UnixStream,
    tx: &mpsc::Sender<AppCommand>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line)?;
    }

    let req: IpcRequest = serde_json::from_str(&line)?;

    let response = match req {
        IpcRequest::Set { name, value } => {
            tx.send(AppCommand::Set { name, value })?;
            IpcResponse::Ok
        }
        IpcRequest::DisplayShader { fragment_glsl } => {
            tx.send(AppCommand::Shader { fragment_glsl })?;
            IpcResponse::Ok
        }
        IpcRequest::StateShader { fragment_glsl } => {
            tx.send(AppCommand::Shader { fragment_glsl })?;
            IpcResponse::Ok
        }
        IpcRequest::InitShader { fragment_glsl } => {
            tx.send(AppCommand::Shader { fragment_glsl })?;
            IpcResponse::Ok
        }
        IpcRequest::Get { name } => {
            // Keep Get simple: read current state via a second lightweight mechanism
            // would require a shared state visible to the IPC thread.
            // For now, reply that Get is unsupported in the server thread.
            IpcResponse::Err {
                message: format!("get '{}' is not implemented in this version", name),
            }
        }
        IpcRequest::Ping => IpcResponse::Pong,
    };

    serde_json::to_writer(&mut stream, &response)?;
    writeln!(&mut stream)?;
    Ok(())
}

pub fn socket_path() -> PathBuf {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    runtime_dir.join(SOCKET_NAME)
}

pub fn send_request_and_print(req: &IpcRequest) -> Result<(), Box<dyn std::error::Error>> {
    let socket = socket_path();
    let mut stream = UnixStream::connect(&socket).map_err(|e| {
        format!(
            "failed to connect to running instance at '{}': {e}",
            socket.display()
        )
    })?;

    serde_json::to_writer(&mut stream, req)?;
    writeln!(&mut stream)?;

    let mut line = String::new();
    BufReader::new(&mut stream).read_line(&mut line)?;

    let resp: IpcResponse = serde_json::from_str(&line)?;
    match resp {
        IpcResponse::Ok => {
            println!("ok");
            Ok(())
        }
        IpcResponse::Value { name, value } => {
            println!("{name}={value}");
            Ok(())
        }
        IpcResponse::Pong => {
            println!("pong");
            Ok(())
        }
        IpcResponse::Err { message } => Err(message.into()),
    }
}
