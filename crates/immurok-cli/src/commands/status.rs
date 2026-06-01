//! `immurok-cli status` — show connection status, pairing, battery, firmware version.

use crate::socket_client::DaemonClient;

pub fn run() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    // STATUS
    let status_rsp = client.send("STATUS").unwrap_or_else(|e| {
        eprintln!("Failed to query status: {}", e);
        std::process::exit(1);
    });

    let parts: Vec<&str> = status_rsp.split(':').collect();
    if parts.first() == Some(&"STATUS") && parts.len() >= 5 {
        let connected = parts[1] == "1";
        let name = parts[2];
        let battery = parts[3];
        let version = parts[4];

        println!("Device:     {}", if name.is_empty() { "-" } else { name });
        println!(
            "Status:     {}",
            if connected {
                "\x1b[32mConnected\x1b[0m"
            } else {
                "\x1b[31mDisconnected\x1b[0m"
            }
        );
        println!(
            "Battery:    {}",
            if battery == "0" && !connected {
                "-".to_string()
            } else {
                format!("{}%", battery)
            }
        );
        println!(
            "Firmware:   {}",
            if version.is_empty() { "-" } else { version }
        );
    } else {
        println!("Status:     {}", status_rsp);
    }

    // PAIR:STATUS
    let pair_rsp = client.send("PAIR:STATUS").unwrap_or_default();
    let pair_parts: Vec<&str> = pair_rsp.split(':').collect();
    let paired = pair_parts.get(1) == Some(&"PAIRED");
    println!(
        "Paired:     {}",
        if paired {
            "\x1b[32mYes\x1b[0m"
        } else {
            "\x1b[33mNo\x1b[0m"
        }
    );

    // FP:LIST (only if connected)
    if parts.first() == Some(&"STATUS") && parts.len() >= 2 && parts[1] == "1" {
        let fp_rsp = client.send("FP:LIST").unwrap_or_default();
        let fp_parts: Vec<&str> = fp_rsp.split(':').collect();
        if fp_parts.first() == Some(&"OK") && fp_parts.len() > 1 {
            if let Ok(bitmap) = fp_parts[1].parse::<u8>() {
                let display = immurok_common::types::fp_bitmap_display(bitmap);
                println!("Fingers:    {}", display);
            }
        }
    }
}
