//! TUI rendering — ratatui widgets for the immurok panel.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};

use super::app::{App, KeyTab, MessageStyle, Mode, PAM_SERVICES, SOUND_PRESETS};
use immurok_common::protocol;

const PRIMARY: Color = Color::Cyan;
const HOTKEY: Color = Color::Cyan;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const ERR: Color = Color::Red;
const DIM: Color = Color::DarkGray;

/// Draw the full TUI frame.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    match app.mode {
        Mode::Keys | Mode::KeyGenInput | Mode::KeyDeleteConfirm => {
            draw_keys(f, app, area);
            return;
        }
        Mode::Pam => {
            draw_main(f, app, area);
            draw_pam_overlay(f, app, area);
            return;
        }
        Mode::Help => {
            draw_main(f, app, area);
            draw_help_overlay(f, area);
            return;
        }
        Mode::Logs => {
            draw_logs(f, app, area);
            return;
        }
        _ => draw_main(f, app, area),
    }
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    // Reserve an extra line for enrollment progress when in-flight.
    let enroll_row = if app.enroll_active { 3 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),              // Status panel
            Constraint::Length(5),              // Fingerprints + settings + PAM
            Constraint::Length(enroll_row),     // Enrollment progress (optional)
            Constraint::Length(4),              // Hotkeys
            Constraint::Min(1),                 // Message
        ])
        .split(area);

    draw_status(f, app, chunks[0]);
    draw_settings(f, app, chunks[1]);
    if app.enroll_active {
        draw_enroll_progress(f, app, chunks[2]);
    }
    draw_hotkeys(f, app, chunks[3]);
    draw_message(f, app, chunks[4]);
}

// ── Status panel ─────────────────────────────────────────────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" immurok{}", if app.daemon_ok { "" } else { " · daemon offline" }),
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if !app.daemon_ok {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  daemon not running — start with `systemctl --user start immurok-daemon`",
                Style::default().fg(ERR),
            )),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let conn_style = if app.connected {
        Style::default().fg(OK).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ERR)
    };
    let conn_text = if app.connected {
        "● Connected"
    } else {
        "○ Disconnected"
    };

    let name = if app.device_name.is_empty() {
        "-"
    } else {
        &app.device_name
    };
    let fw = if app.fw_version.is_empty() {
        "-"
    } else {
        app.fw_version.as_str()
    };
    let paired_style = if app.paired {
        Style::default().fg(OK).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(WARN)
    };
    let paired_text = if app.paired { "Yes" } else { "No" };

    // Battery bar
    let (batt_str, batt_bar, batt_style) = battery_render(app);

    let lines = vec![
        Line::from(vec![
            Span::raw("  Device   "),
            Span::styled(
                format!("{:<22}", name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("Battery  "),
            Span::styled(batt_bar, batt_style),
            Span::raw("  "),
            Span::styled(batt_str, batt_style),
        ]),
        Line::from(vec![
            Span::raw("  Status   "),
            Span::styled(format!("{:<22}", conn_text), conn_style),
            Span::raw("Firmware "),
            Span::raw(fw),
        ]),
        Line::from(vec![
            Span::raw("  Paired   "),
            Span::styled(format!("{:<22}", paired_text), paired_style),
        ]),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

/// Returns (label, bar, style). Style turns red below 15%.
fn battery_render(app: &App) -> (String, String, Style) {
    if !app.connected || app.battery == 0 {
        return ("-".into(), "▱▱▱▱▱".into(), Style::default().fg(DIM));
    }
    let pct = app.battery.min(100);
    let filled = ((pct as usize * 5) + 50) / 100;
    let filled = filled.min(5);
    let mut bar = String::with_capacity(5);
    for _ in 0..filled {
        bar.push('▰');
    }
    for _ in 0..(5 - filled) {
        bar.push('▱');
    }
    let color = if pct < 15 {
        ERR
    } else if pct < 30 {
        WARN
    } else {
        OK
    };
    (format!("{}%", pct), bar, Style::default().fg(color).add_modifier(Modifier::BOLD))
}

// ── Settings panel (fingerprints + toggles + PAM) ──────────

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(
                " Slots & settings  ({}/{} enrolled)",
                fp_count(app.fp_bitmap),
                protocol::MAX_FINGERPRINT_SLOTS
            ),
            Style::default().fg(PRIMARY),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Row 1: fingerprint slots
    let mut fp_spans = vec![Span::raw("  Slots   ")];
    for i in 0..protocol::MAX_FINGERPRINT_SLOTS {
        let has = app.fp_bitmap & (1 << i) != 0;
        if has {
            fp_spans.push(Span::styled(
                format!("[{}●]", i),
                Style::default().fg(OK).add_modifier(Modifier::BOLD),
            ));
        } else {
            fp_spans.push(Span::styled(
                format!("[{}○]", i),
                Style::default().fg(DIM),
            ));
        }
        fp_spans.push(Span::raw(" "));
    }

    // Row 2: feature toggles
    let sound_label = if app.unlock_sound.is_empty() {
        "silent".to_string()
    } else {
        app.unlock_sound.clone()
    };

    let toggles_line = Line::from(vec![
        Span::raw("  Auth   "),
        on_off("sudo", app.unlock_sudo),
        Span::raw("  "),
        on_off("polkit", app.unlock_polkit),
        Span::raw("  "),
        on_off("screen", app.unlock_screen),
        Span::raw("  "),
        on_off("long-press lock", app.lock_screen),
        Span::raw("  sound:"),
        Span::styled(
            format!(" {}", sound_label),
            Style::default()
                .fg(if app.unlock_sound.is_empty() { DIM } else { PRIMARY })
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    // Row 3: PAM status
    let pam_line = Line::from(vec![
        Span::raw("  PAM    "),
        pam_chip("sudo", app.pam_sudo),
        Span::raw("  "),
        pam_chip("polkit", app.pam_polkit),
        Span::raw("  "),
        pam_chip("gdm", app.pam_screen),
    ]);

    let lines = vec![Line::from(fp_spans), toggles_line, pam_line];
    f.render_widget(Paragraph::new(lines), inner);
}

fn on_off<'a>(label: &'a str, on: bool) -> Span<'a> {
    let style = if on {
        Style::default().fg(OK).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    Span::styled(
        format!("{}:{}", label, if on { "on" } else { "off" }),
        style,
    )
}

fn pam_chip<'a>(label: &'a str, installed: bool) -> Span<'a> {
    let (sym, color) = if installed { ("✓", OK) } else { ("✗", DIM) };
    Span::styled(
        format!("{} {}", sym, label),
        Style::default().fg(color),
    )
}

fn fp_count(bitmap: u8) -> u32 {
    (bitmap & 0x1F).count_ones()
}

// ── Enrollment progress bar ─────────────────────────────────

fn draw_enroll_progress(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Enrolling slot {} ", app.enroll_slot),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(WARN));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = app.enroll_total.max(1) as u16;
    let cur = app.enroll_current.min(app.enroll_total) as u16;
    let ratio = (cur as f64) / (total as f64);
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(WARN).bg(Color::Reset))
        .ratio(ratio.clamp(0.0, 1.0))
        .label(format!(
            "{}/{}  ·  Esc to cancel",
            app.enroll_current, app.enroll_total
        ));
    f.render_widget(gauge, inner);
}

// ── Hotkeys ──────────────────────────────────────────────────

fn draw_hotkeys(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PRIMARY));

    let key_style = Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD);

    let lines = match app.mode {
        Mode::Normal | Mode::Help | Mode::Pam => vec![
            Line::from(vec![
                Span::raw("  "),
                hotkey("p", "air", key_style),
                Span::raw("  "),
                hotkey("u", "npair", key_style),
                Span::raw("  "),
                hotkey("e", "nroll", key_style),
                Span::raw("  "),
                hotkey("d", "elete", key_style),
                Span::raw("  "),
                hotkey("v", "erify", key_style),
                Span::raw("  "),
                hotkey("s", "udo", key_style),
                Span::raw("  "),
                hotkey("o", "polkit", key_style),
                Span::raw("  "),
                hotkey("k", "screen", key_style),
                Span::raw("  "),
                hotkey("L", "ock", key_style),
                Span::raw("  "),
                hotkey("n", "sound", key_style),
            ]),
            Line::from(vec![
                Span::raw("  "),
                hotkey("K", "eys", key_style),
                Span::raw("  "),
                hotkey("M", "PAM", key_style),
                Span::raw("  "),
                hotkey("i", "nfo", key_style),
                Span::raw("  "),
                hotkey("l", "ogs", key_style),
                Span::raw("  "),
                hotkey("?", "help", key_style),
                Span::raw("  "),
                hotkey("q", "uit", key_style),
            ]),
        ],
        Mode::EnrollSelect => vec![Line::from(Span::styled(
            format!(
                "  Press [0-{}] to choose enroll slot · [Esc] cancel",
                protocol::MAX_FINGERPRINT_SLOTS - 1
            ),
            Style::default().fg(WARN),
        ))],
        Mode::DeleteSelect => vec![Line::from(Span::styled(
            format!(
                "  Press [0-{}] to choose delete slot · [Esc] cancel",
                protocol::MAX_FINGERPRINT_SLOTS - 1
            ),
            Style::default().fg(WARN),
        ))],
        // Keys / KeyGenInput / KeyDeleteConfirm are rendered by draw_keys;
        // Logs has its own footer in draw_logs — neither path reaches here,
        // but the arm is required for exhaustiveness.
        Mode::Keys | Mode::KeyGenInput | Mode::KeyDeleteConfirm | Mode::Logs => Vec::new(),
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn hotkey<'a>(key: &'a str, rest: &'a str, key_style: Style) -> Span<'a> {
    // Returns a *single* Span — caller chains them. We embed the key in
    // brackets with style, but Span can't carry mixed styles. So we use
    // styled-on-key trick by relying on caller composing multiple Spans.
    // Here we just hand back the [k]rest combo as one styled span.
    Span::styled(format!("[{}]{}", key, rest), key_style)
}

// ── Message area ─────────────────────────────────────────────

fn draw_message(f: &mut Frame, app: &App, area: Rect) {
    let style = match app.message_style {
        MessageStyle::Dim => Style::default().fg(DIM),
        MessageStyle::Green => Style::default().fg(OK).add_modifier(Modifier::BOLD),
        MessageStyle::Red => Style::default().fg(ERR).add_modifier(Modifier::BOLD),
        MessageStyle::Yellow => Style::default().fg(WARN).add_modifier(Modifier::BOLD),
    };
    f.render_widget(
        Paragraph::new(format!("  {}", app.message))
            .style(style)
            .wrap(Wrap { trim: false }),
        area,
    );
}

// ── Keys panel ───────────────────────────────────────────────

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tabs / title
            Constraint::Min(3),    // Key list
            Constraint::Length(4), // Hotkeys / input
            Constraint::Length(3), // Message
        ])
        .split(area);

    // ── Tabs row ─────────────────────────────────────────────
    let tab_block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " immurok / keys ",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let active = Style::default()
        .fg(Color::Black)
        .bg(PRIMARY)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(DIM);

    let mk_tab = |label: &str, count: usize, max: u8, is_active: bool| -> Span<'_> {
        let txt = format!(" {} ({}/{}) ", label, count, max);
        Span::styled(txt, if is_active { active } else { inactive })
    };

    let tabs_line = Line::from(vec![
        Span::raw("  "),
        mk_tab(
            "1·SSH",
            app.ssh_keys.len(),
            protocol::KEY_MAX_SSH,
            app.key_tab == KeyTab::Ssh,
        ),
        Span::raw(" "),
        mk_tab(
            "2·OTP",
            app.otp_keys.len(),
            protocol::KEY_MAX_OTP,
            app.key_tab == KeyTab::Otp,
        ),
        Span::raw(" "),
        mk_tab(
            "3·API",
            app.api_keys.len(),
            protocol::KEY_MAX_API,
            app.key_tab == KeyTab::Api,
        ),
    ]);
    f.render_widget(Paragraph::new(tabs_line).block(tab_block), chunks[0]);

    // ── Key list ─────────────────────────────────────────────
    let list_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(PRIMARY));

    let list_area = list_block.inner(chunks[1]);
    f.render_widget(list_block, chunks[1]);

    let visible_rows = list_area.height as usize;
    let cur = app.key_cursor;
    let total = app.current_key_len();

    let start = if total <= visible_rows {
        0
    } else if cur < visible_rows / 2 {
        0
    } else if cur + visible_rows / 2 >= total {
        total.saturating_sub(visible_rows)
    } else {
        cur.saturating_sub(visible_rows / 2)
    };
    let end = (start + visible_rows).min(total);

    let mut lines: Vec<Line<'_>> = Vec::new();
    if total == 0 {
        let hint = if !app.connected {
            "  (no entries cached — connect device to sync)"
        } else {
            "  (no entries)"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(DIM),
        )));
    } else {
        for idx_in_list in start..end {
            let is_sel = idx_in_list == cur;
            let marker = if is_sel { "▶ " } else { "  " };
            let row_style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(PRIMARY)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let line = match app.key_tab {
                KeyTab::Ssh => {
                    let r = &app.ssh_keys[idx_in_list];
                    let fp = if r.fingerprint.is_empty() {
                        "-".to_string()
                    } else {
                        r.fingerprint.clone()
                    };
                    Line::from(Span::styled(
                        format!(
                            "{}[{:>2}] {:<18}  {}",
                            marker,
                            r.index,
                            truncate(&r.name, 18),
                            fp
                        ),
                        row_style,
                    ))
                }
                KeyTab::Otp => {
                    let r = &app.otp_keys[idx_in_list];
                    Line::from(Span::styled(
                        format!("{}[{:>2}] {}", marker, r.index, r.name),
                        row_style,
                    ))
                }
                KeyTab::Api => {
                    let r = &app.api_keys[idx_in_list];
                    Line::from(Span::styled(
                        format!("{}[{:>2}] {}", marker, r.index, r.name),
                        row_style,
                    ))
                }
            };
            lines.push(line);
        }
    }
    f.render_widget(Paragraph::new(lines), list_area);

    // ── Hotkeys / input ──────────────────────────────────────
    let key_style = Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD);
    let hot_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PRIMARY));

    let hot_text = match app.mode {
        Mode::KeyGenInput => vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(WARN)),
                Span::styled(&app.input_buf, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    "_",
                    Style::default().fg(PRIMARY).add_modifier(Modifier::SLOW_BLINK),
                ),
            ]),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("[Enter]", key_style),
                Span::raw(" confirm   "),
                Span::styled("[Esc]", key_style),
                Span::raw(" cancel   (max 15 chars)"),
            ]),
        ],
        Mode::KeyDeleteConfirm => vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled("[y]", key_style),
                Span::raw(" / "),
                Span::styled("[Enter]", key_style),
                Span::raw(" confirm    "),
                Span::styled("[n]", key_style),
                Span::raw(" / "),
                Span::styled("[Esc]", key_style),
                Span::raw(" cancel"),
            ]),
            Line::from(""),
        ],
        _ => vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled("[Tab/1-3]", key_style),
                Span::raw(" tab  "),
                Span::styled("[↑↓/jk]", key_style),
                Span::raw(" move  "),
                Span::styled("[g]", key_style),
                Span::raw("en  "),
                Span::styled("[d]", key_style),
                Span::raw("el  "),
                Span::styled("[o]", key_style),
                Span::raw("tp  "),
                Span::styled("[c]", key_style),
                Span::raw("opy  "),
                Span::styled("[r]", key_style),
                Span::raw("efresh  "),
                Span::styled("[Esc]", key_style),
                Span::raw(" back"),
            ]),
            Line::from(Span::styled(
                match app.key_tab {
                    KeyTab::Ssh => "  [g] generate SSH keypair  ·  [c] show authorized_keys line",
                    KeyTab::Otp => "  [o] fetch TOTP code (FP-gated)  ·  add OTP via `immurok-cli key add otp`",
                    KeyTab::Api => "  add API key via `immurok-cli key add api`",
                },
                Style::default().fg(DIM),
            )),
        ],
    };
    f.render_widget(Paragraph::new(hot_text).block(hot_block), chunks[2]);

    draw_message(f, app, chunks[3]);
}

// ── Help overlay ─────────────────────────────────────────────

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 80, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Help ",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let key = |k: &'static str| {
        Span::styled(
            format!("{:<10}", k),
            Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD),
        )
    };

    let lines = vec![
        Line::from(Span::styled(
            "Pairing & fingerprints",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![key("p"), Span::raw("Pair device (press button on device)")]),
        Line::from(vec![key("u"), Span::raw("Unpair / factory reset")]),
        Line::from(vec![key("e"), Span::raw("Enroll: pick slot 0-4")]),
        Line::from(vec![key("E"), Span::raw("Enroll into first empty slot")]),
        Line::from(vec![key("d"), Span::raw("Delete slot (pick 0-4)")]),
        Line::from(vec![key("v"), Span::raw("Verify fingerprint")]),
        Line::from(vec![key("Esc"), Span::raw("Cancel in-flight enrollment")]),
        Line::from(""),
        Line::from(Span::styled(
            "Settings",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![key("s"), Span::raw("Toggle sudo unlock")]),
        Line::from(vec![key("o"), Span::raw("Toggle polkit unlock")]),
        Line::from(vec![key("k"), Span::raw("Toggle screen unlock")]),
        Line::from(vec![key("L"), Span::raw("Toggle long-press → lock screen")]),
        Line::from(vec![key("n"), Span::raw("Cycle unlock sound preset")]),
        Line::from(""),
        Line::from(Span::styled(
            "Panels",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![key("K"), Span::raw("Keys (SSH / OTP / API)")]),
        Line::from(vec![key("M"), Span::raw("PAM services install / remove")]),
        Line::from(vec![key("i"), Span::raw("Device info one-liner")]),
        Line::from(vec![key("l"), Span::raw("Tail daemon logs in-TUI (Esc to return)")]),
        Line::from(""),
        Line::from(Span::styled(
            "Press ? or Esc to close.",
            Style::default().fg(DIM),
        )),
    ];

    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ── PAM overlay ──────────────────────────────────────────────

fn draw_pam_overlay(f: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(60, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " PAM services ",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(inner);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  pkexec will prompt for an admin password.",
        Style::default().fg(DIM),
    )));
    lines.push(Line::from(""));

    for (i, svc) in PAM_SERVICES.iter().enumerate() {
        let is_sel = i == app.pam_cursor;
        let installed = app.pam_is_installed(i);
        let (sym, sym_color) = if installed { ("✓", OK) } else { ("✗", DIM) };
        let status = if installed { "installed" } else { "not installed" };
        let marker = if is_sel { "▶ " } else { "  " };
        let row_style = if is_sel {
            Style::default()
                .fg(Color::Black)
                .bg(PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(marker, row_style),
            Span::styled(
                format!("{:<14}", svc.display),
                row_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", sym), Style::default().fg(sym_color)),
            Span::styled(
                status.to_string(),
                Style::default().fg(if installed { OK } else { DIM }),
            ),
            Span::styled(format!("  /etc/pam.d/{}", svc.service), Style::default().fg(DIM)),
        ]));
    }

    let footer = Line::from(vec![
        Span::raw("  "),
        Span::styled("[i]", Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
        Span::raw("nstall  "),
        Span::styled("[r]", Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
        Span::raw("emove  "),
        Span::styled("[↑↓/jk]", Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
        Span::raw(" move  "),
        Span::styled("[Esc]", Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
        Span::raw(" back"),
    ]);

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

// ── Logs panel ───────────────────────────────────────────────

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(3),    // Log body
            Constraint::Length(3), // Hotkey footer
        ])
        .split(area);

    // ── Header ────────────────────────────────────────────────
    let live = app.log_child.is_some();
    let tail_indicator = if !live {
        Span::styled("● stream closed", Style::default().fg(ERR))
    } else if app.log_scroll == 0 {
        Span::styled(
            "● live tail",
            Style::default().fg(OK).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!("⏸ scrolled back {} lines", app.log_scroll),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )
    };

    let header_block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Daemon logs · immurok-daemon ",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY));

    let header_line = Line::from(vec![
        Span::raw("  "),
        tail_indicator,
        Span::styled(
            format!("    {} lines buffered", app.log_lines.len()),
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(header_line).block(header_block), chunks[0]);

    // ── Body ──────────────────────────────────────────────────
    let body_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(PRIMARY));
    let body_inner = body_block.inner(chunks[1]);
    f.render_widget(body_block, chunks[1]);

    let visible = body_inner.height as usize;
    let total = app.log_lines.len();
    let end = total.saturating_sub(app.log_scroll);
    let start = end.saturating_sub(visible);

    let lines: Vec<Line<'_>> = if total == 0 {
        vec![Line::from(Span::styled(
            "  (waiting for daemon output…)",
            Style::default().fg(DIM),
        ))]
    } else {
        app.log_lines
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|s| {
                let style = log_line_style(s);
                Line::from(Span::styled(s.clone(), style))
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), body_inner);

    // ── Footer ────────────────────────────────────────────────
    let footer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PRIMARY));

    let key = Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD);
    let footer = Line::from(vec![
        Span::raw("  "),
        Span::styled("[↑↓/jk]", key),
        Span::raw(" line  "),
        Span::styled("[PgUp/PgDn]", key),
        Span::raw(" page  "),
        Span::styled("[Home]", key),
        Span::raw(" top  "),
        Span::styled("[End]", key),
        Span::raw(" follow tail  "),
        Span::styled("[Esc/q]", key),
        Span::raw(" back"),
    ]);
    f.render_widget(Paragraph::new(footer).block(footer_block), chunks[2]);
}

/// Color-code log lines: ERROR red, WARN yellow, INFO default.
/// Matches the level tokens journalctl prints for tracing-subscriber output.
fn log_line_style(line: &str) -> Style {
    // Cheap substring scan — fine at our line rate (≤200 lines/sec).
    if line.contains(" ERROR ")
        || line.contains("ERROR:")
        || line.contains("[ERROR]")
        || line.contains(" panicked")
    {
        Style::default().fg(ERR).add_modifier(Modifier::BOLD)
    } else if line.contains(" WARN ") || line.contains("WARN:") || line.contains("[WARN]") {
        Style::default().fg(WARN)
    } else if line.contains(" DEBUG ") || line.contains(" TRACE ") {
        Style::default().fg(DIM)
    } else {
        Style::default()
    }
}

// ── helpers ─────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Compute a centered rect with given percent width/height of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

// ── unused but kept for reference: SOUND_PRESETS cycle hint ──
// (silenced compile warning by using it in a helper)
#[allow(dead_code)]
fn sound_presets_hint() -> String {
    let mut out = String::new();
    for (i, s) in SOUND_PRESETS.iter().enumerate() {
        if i > 0 {
            out.push_str(" → ");
        }
        out.push_str(if s.is_empty() { "silent" } else { s });
    }
    out
}
