//! `immurok-cli info` — detailed device information.

use crate::socket_client::DaemonClient;

pub fn run() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("GET:INFO").unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to query info: {}", e));
    });

    let parts: Vec<&str> = rsp.split(':').collect();
    if parts.first() != Some(&"OK") {
        super::error_exit(&format!("Unexpected response: {}", rsp));
    }

    let mut info = std::collections::HashMap::new();
    for part in &parts[1..] {
        if let Some((k, v)) = part.split_once('=') {
            info.insert(k, v);
        }
    }

    println!("Model:      {}", info.get("model").unwrap_or(&"-"));
    println!("Firmware:   {}", info.get("fw").unwrap_or(&"-"));
    println!("Connected:  {}", if info.get("connected") == Some(&"1") { "Yes" } else { "No" });

    let battery = info.get("battery").unwrap_or(&"-1");
    if *battery != "-1" {
        println!("Battery:    {}%", battery);
    } else {
        println!("Battery:    -");
    }
}
