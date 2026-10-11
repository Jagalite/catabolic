// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Disposable-loopback qualification entry point, not a default server backend.
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        eprintln!("usage: catabolic-http-experimental DATABASE PORT read-only|writable");
        std::process::exit(2)
    }
    let config = catabolic_http::ServerConfig {
        port: args[2].parse().unwrap_or_else(|_| {
            eprintln!("invalid port");
            std::process::exit(2)
        }),
        read_only: match args[3].as_str() {
            "read-only" => true,
            "writable" => false,
            _ => {
                eprintln!("invalid mode");
                std::process::exit(2)
            }
        },
        ..Default::default()
    };
    if let Err(error) = catabolic_http::serve(std::path::Path::new(&args[1]), config) {
        eprintln!("{error}");
        std::process::exit(2)
    }
}
