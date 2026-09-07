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
        // 第 7 段：BlueZ 说设备 Connected，但系统层配对从没做成（老 daemon 没有
        // 这一段）。不点明的话，这里只会报一个跟操作系统说法直接矛盾的
        // Disconnected —— 那是用户唯一无法据以行动的状态。
        let link_unbonded = parts.get(6) == Some(&"1");

        println!("Device:     {}", if name.is_empty() { "-" } else { name });
        println!(
            "Status:     {}",
            if connected {
                "\x1b[32mConnected\x1b[0m"
            } else if link_unbonded {
                "\x1b[31mDisconnected\x1b[0m \x1b[33m(connected to the OS, not bonded)\x1b[0m"
            } else {
                "\x1b[31mDisconnected\x1b[0m"
            }
        );
        if link_unbonded {
            println!(
                "\x1b[31m⚠ The device is connected to this computer but was never bonded.\x1b[0m"
            );
            println!("  BlueZ reports Paired=false and ServicesResolved=false. That blocks every");
            println!("  route the daemon has to the device, and it will not recover on its own —");
            println!("  which is why this line says Disconnected while your OS says connected.");
            println!("  Pair the device at the OS level first. On a desktop with no Bluetooth");
            println!("  applet, a pairing agent must be running to answer its confirmation:");
            println!("    \x1b[1mbt-agent -c DisplayYesNo &\x1b[0m");
            println!("    \x1b[1mbluetoothctl pair <address>\x1b[0m");
            println!("  It has to be DisplayYesNo — the device refuses NoInputNoOutput.");
        }
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
        if !version.is_empty() {
            use immurok_common::fwupdate::version::{normalize_semver, FirmwareVersion};
            let norm = normalize_semver(version);
            if let (Some(v), Some(min)) = (
                FirmwareVersion::parse(&norm),
                FirmwareVersion::parse(crate::fwupdate::MANDATORY_MIN_VERSION),
            ) {
                if v < min {
                    println!(
                        "\x1b[33mWarning: firmware outdated (old signing era) — run `immurok-cli fw update`\x1b[0m"
                    );
                }
            }
        }
    } else {
        println!("Status:     {}", status_rsp);
    }

    // PAIR:STATUS — needs a fresh connection: the daemon serves exactly one
    // request per connection, so reusing `client` reads EOF (always "No").
    let pair_rsp = DaemonClient::connect()
        .and_then(|mut c| c.send("PAIR:STATUS"))
        .unwrap_or_default();
    let pair_parts: Vec<&str> = pair_rsp.split(':').collect();
    let paired = pair_parts.get(1) == Some(&"PAIRED");
    // STATUS 的第 6 段是设备自己说的「我没和你配对」（老 daemon 没有这一段）。
    // 本机说已配对、设备说没有，就是设备被工厂复位（或本机的槽在别处被清）后
    // 的 split state：认证会全部静默失败，而在此之前这一行会一直显示 Yes。
    let device_unpaired = parts.get(5) == Some(&"1");
    println!(
        "Paired:     {}",
        if device_unpaired {
            "\x1b[31mNo (device says so)\x1b[0m"
        } else if paired {
            "\x1b[32mYes\x1b[0m"
        } else {
            "\x1b[33mNo\x1b[0m"
        }
    );
    if device_unpaired {
        println!(
            "\x1b[31m⚠ The device is no longer paired with this computer.\x1b[0m"
        );
        println!(
            "  It was probably factory-reset, or this computer's slot was cleared"
        );
        println!(
            "  from the other host. Fingerprint auth will keep falling back to your"
        );
        println!("  password until you run: \x1b[1mimmurok-cli pair\x1b[0m");
    }

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
