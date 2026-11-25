mod config;
mod detect;
mod document;
mod server;
mod transport;
mod typeset;

use std::io;
use std::process::ExitCode;

use serde_json::Value;

use server::{Server, Step, parse_error};

fn run() -> io::Result<u8> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut server = Server::new();
    while let Some(body) = transport::read(&mut input)? {
        let step = match serde_json::from_slice::<Value>(&body) {
            Ok(message) => server.handle(&message),
            Err(_) => Step { out: vec![parse_error()], exit: None },
        };
        for message in &step.out {
            transport::write(&mut output, message)?;
        }
        if let Some(code) = step.exit {
            return Ok(code);
        }
    }
    Ok(1)
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| eprintln!("microtypo-lsp: internal error")));
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None | Some("--stdio"), None) => {}
        (Some("--version"), None) => {
            println!("microtypo-lsp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("usage: microtypo-lsp [--stdio | --version]");
            return ExitCode::from(2);
        }
    }
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("microtypo-lsp: transport: {}", error.kind());
            ExitCode::FAILURE
        }
    }
}
