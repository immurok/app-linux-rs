//! TUI rendering — ratatui widgets for the immurok panel.
//!
//! Layout (all tabs):
//! ```text
//!   immurok                     ● Connected · immurok · FW 1.6.1 · ▰▰▰▰▱ 71%
//!   ❬1❭ Dashboard   ❬2❭ Keys   ❬3❭ PAM   ❬4❭ Logs
//!  ┌─ tab content ─────────────────────────────────────────────────────────┐
//!  │ …                                                                     │
//!  └───────────────────────────────────────────────────────────────────────┘
//!   ✓ Ready                                    ← message line
//!   p pair · e enroll · … · q quit             ← context hotkeys
//! ```

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};

use super::app::{App, KeyInputStage, KeyTab, MessageStyle, Mode, Tab, PAM_SERVICES};
use immurok_common::protocol;

const PRIMARY: Color = Color::Cyan;
const HOTKEY: Color = Color::Cyan;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const ERR: Color = Color::Red;
const DIM: Color = Color::DarkGray;

/// Minimum terminal width at which the Dashboard shows the Events column.
const EVENTS_MIN_WIDTH: u16 = 84;

/// Draw the full TUI frame.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header line
            Constraint::Length(1), // Tab bar
            Constraint::Min(3),    // Content
            Constraint::Length(1), // Message line
            Constraint::Length(1), // Hotkey line
        ])
        .split(area);

    draw_header(f, app, chunks[0]);
    draw_tabbar(f, app, chunks[1]);

    match app.tab {
        Tab::Dashboard => draw_dashboard(f, app, chunks[2]),
        Tab::Keys => draw_keys(f, app, chunks[2]),
        Tab::Pam => draw_pam(f, app, chunks[2]),
        Tab::Logs => draw_logs(f, app, chunks[2]),
    }

    draw_message(f, app, chunks[3]);
    draw_hotkeys(f, app, chunks[4]);

    if app.mode == Mode::Help {
        draw_help_overlay(f, area);
    }
}

// ── Header line ──────────────────────────────────────────────

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    // Brand, left-aligned.
    let brand = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "immurok",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(brand, area);

    // Status summary, right-aligned.
    let mut spans: Vec<Span> = Vec::new();
    if !app.daemon_ok {
        spans.push(Span::styled(
            "○ daemon offline",
            Style::default().fg(ERR).add_modifier(Modifier::BOLD),
        ));
    } else if !app.connected {
        spans.push(Span::styled("○ Disconnected", Style::default().fg(ERR)));
        if app.paired {
            spans.push(Span::styled("  ·  paired", Style::default().fg(DIM)));
        } else {
            spans.push(Span::styled("  ·  not paired", Style::default().fg(WARN)));
        }
    } else {
        spans.push(Span::styled(
            "● Connected",
            Style::default().fg(OK).add_modifier(Modifier::BOLD),
        ));
        if !app.device_name.is_empty() {
            spans.push(Span::styled("  ·  ", Style::default().fg(DIM)));
            spans.push(Span::styled(
                app.device_name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        if !app.paired {
            spans.push(Span::styled("  ·  ", Style::default().fg(DIM)));
            spans.push(Span::styled("not paired", Style::default().fg(WARN)));
        }
        if !app.fw_version.is_empty() {
            spans.push(Span::styled("  ·  ", Style::default().fg(DIM)));
            spans.push(Span::styled(
                format!("FW {}", app.fw_version),
                Style::default().fg(DIM),
            ));
        }
        let (batt_str, batt_bar, batt_style) = battery_render(app);
        spans.push(Span::styled("  ·  ", Style::default().fg(DIM)));
        spans.push(Span::styled(batt_bar, batt_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(batt_str, batt_style));
    }
    spans.push(Span::raw(" "));

    let status = Paragraph::new(Line::from(spans)).alignment(Alignment::Right);
    f.render_widget(status, area);
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
    (
        format!("{}%", pct),
        bar,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

// ── Tab bar ──────────────────────────────────────────────────

fn draw_tabbar(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for tab in Tab::ALL {
        let is_active = app.tab == tab;
        let key_style = if is_active {
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let label_style = if is_active {
            Style::default()
                .fg(PRIMARY)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(format!("❬{}❭", tab.hotkey()), key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(tab.title(), label_style));
        spans.push(Span::raw("   "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Dashboard tab ────────────────────────────────────────────

fn draw_dashboard(f: &mut Frame, app: &App, area: Rect) {
    if !app.daemon_ok {
        let block = content_block(" Dashboard ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  daemon not running",
                Style::default().fg(ERR).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  start it with:  systemctl --user start immurok-daemon",
                Style::default().fg(DIM),
            )),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Wide terminals get a right-hand Events column.
    let show_events = area.width >= EVENTS_MIN_WIDTH;
    let cols = if show_events {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(46), Constraint::Min(20)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20)])
            .split(area)
    };

    draw_dashboard_left(f, app, cols[0]);
    if show_events {
        draw_events(f, app, cols[1]);
    }
}

fn draw_dashboard_left(f: &mut Frame, app: &App, area: Rect) {
    // Fingerprints block grows by 2 rows while enrolling (hint + gauge).
    let fp_height = if app.enroll_active { 5 } else { 3 };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(fp_height), // Fingerprints
            Constraint::Length(7),         // Unlock toggles
            Constraint::Length(3),         // PAM summary
            Constraint::Min(0),            // Filler
        ])
        .split(area);

    draw_fingerprints(f, app, rows[0]);
    draw_unlock(f, app, rows[1]);
    draw_pam_summary(f, app, rows[2]);
}

fn draw_fingerprints(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.enroll_active {
        format!(" Fingerprints · enrolling slot {} ", app.enroll_slot)
    } else {
        format!(
            " Fingerprints · {}/{} ",
            fp_count(app.fp_bitmap),
            protocol::MAX_FINGERPRINT_SLOTS
        )
    };
    let border = if app.enroll_active { WARN } else { PRIMARY };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(border).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.enroll_active {
            vec![
                Constraint::Length(1), // slots
                Constraint::Length(1), // hint
                Constraint::Length(1), // gauge
            ]
        } else {
            vec![Constraint::Length(1)]
        })
        .split(inner);

    // Slot row
    let mut spans: Vec<Span> = vec![Span::raw("  ")];
    for i in 0..protocol::MAX_FINGERPRINT_SLOTS {
        let has = app.fp_bitmap & (1 << i) != 0;
        let enrolling = app.enroll_active && i == app.enroll_slot;
        if enrolling {
            spans.push(Span::styled(
                format!("◍ {}", i),
                Style::default().fg(WARN).add_modifier(Modifier::BOLD),
            ));
        } else if has {
            spans.push(Span::styled(
                format!("⬤ {}", i),
                Style::default().fg(OK).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(format!("○ {}", i), Style::default().fg(DIM)));
        }
        spans.push(Span::raw("    "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    // Enrollment hint + gauge
    if app.enroll_active {
        use crate::enroll_hint::{step_arrow, step_hint};

        // FP-gate phase: the device wants an already-enrolled finger to
        // authorize — showing capture-pose hints here would mislead.
        if app.enroll_gate {
            let hint = Paragraph::new(Line::from(vec![
                Span::styled("  ⚿ ", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
                Span::raw("Verify with an enrolled finger"),
            ]));
            f.render_widget(hint, rows[1]);

            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(WARN).bg(Color::Reset))
                .ratio(0.0)
                .label("awaiting authorization · Esc to cancel");
            f.render_widget(gauge, rows[2]);
            return;
        }

        let total = app.enroll_total.max(1);
        let step = app.enroll_current.saturating_add(1).min(total);
        let hint = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("  {} ", step_arrow(step)),
                Style::default().fg(WARN).add_modifier(Modifier::BOLD),
            ),
            Span::raw(step_hint(step)),
        ]));
        f.render_widget(hint, rows[1]);

        let ratio = (app.enroll_current.min(app.enroll_total) as f64) / (total as f64);
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(WARN).bg(Color::Reset))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(format!(
                "{}/{} · Esc to cancel",
                app.enroll_current, app.enroll_total
            ));
        f.render_widget(gauge, rows[2]);
    }
}

fn draw_unlock(f: &mut Frame, app: &App, area: Rect) {
    let block = content_block(" Unlock ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (sound_label, sound_style) = if app.unlock_sound.is_empty() {
        ("silent".to_string(), Style::default().fg(DIM))
    } else {
        (
            format!("♪ {}", app.unlock_sound),
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        )
    };

    let lines = vec![
        toggle_row("s", "sudo", app.unlock_sudo),
        toggle_row("o", "polkit", app.unlock_polkit),
        toggle_row("k", "screen unlock", app.unlock_screen),
        toggle_row("L", "long-press lock", app.lock_screen),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("n", Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(format!("{:<17}", "sound")),
            Span::styled(sound_label, sound_style),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn toggle_row<'a>(key: &'a str, label: &'a str, on: bool) -> Line<'a> {
    let state = if on {
        Span::styled("● on", Style::default().fg(OK).add_modifier(Modifier::BOLD))
    } else {
        Span::styled("○ off", Style::default().fg(DIM))
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(key, Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::raw(format!("{:<17}", label)),
        state,
    ])
}

fn draw_pam_summary(f: &mut Frame, app: &App, area: Rect) {
    let block = content_block(" PAM ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let line = Line::from(vec![
        Span::raw("  "),
        pam_chip("sudo", app.pam_sudo),
        Span::raw("   "),
        pam_chip("polkit", app.pam_polkit),
        Span::raw("   "),
        pam_chip("gdm", app.pam_screen),
        Span::styled("      ❬3❭ manage", Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}

fn pam_chip<'a>(label: &'a str, installed: bool) -> Span<'a> {
    let (sym, color) = if installed { ("✓", OK) } else { ("✗", DIM) };
    Span::styled(format!("{} {}", sym, label), Style::default().fg(color))
}

fn fp_count(bitmap: u8) -> u32 {
    (bitmap & 0x1F).count_ones()
}

// ── Events feed (Dashboard right column) ─────────────────────

fn draw_events(f: &mut Frame, app: &App, area: Rect) {
    let block = content_block(" Events ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = inner.height as usize;
    let total = app.events.len();
    let start = total.saturating_sub(visible);

    let lines: Vec<Line<'_>> = if total == 0 {
        vec![Line::from(Span::styled(
            "  (no events yet)",
            Style::default().fg(DIM),
        ))]
    } else {
        app.events
            .iter()
            .skip(start)
            .map(|e| {
                let (sym, color) = match e.style {
                    MessageStyle::Green => ("✓", OK),
                    MessageStyle::Red => ("✗", ERR),
                    MessageStyle::Yellow => ("…", WARN),
                    MessageStyle::Dim => ("·", DIM),
                };
                Line::from(vec![
                    Span::styled(format!(" {} ", e.time), Style::default().fg(DIM)),
                    Span::styled(format!("{} ", sym), Style::default().fg(color)),
                    Span::raw(truncate(&e.text, inner.width.saturating_sub(13) as usize)),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), inner);
}

// ── Keys tab ─────────────────────────────────────────────────

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let block = content_block(" Keys ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // category segments
            Constraint::Length(1), // separator / column header
            Constraint::Min(1),    // list
        ])
        .split(inner);

    // Category segments
    let active = Style::default()
        .fg(Color::Black)
        .bg(PRIMARY)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(DIM);
    let mk_seg = |label: &str, count: usize, max: u8, is_active: bool| -> Span<'_> {
        Span::styled(
            format!(" {} {}/{} ", label, count, max),
            if is_active { active } else { inactive },
        )
    };
    let seg_line = Line::from(vec![
        Span::raw("  "),
        mk_seg("SSH", app.ssh_keys.len(), protocol::KEY_MAX_SSH, app.key_tab == KeyTab::Ssh),
        Span::raw("  "),
        mk_seg("OTP", app.otp_keys.len(), protocol::KEY_MAX_OTP, app.key_tab == KeyTab::Otp),
        Span::raw("  "),
        mk_seg("API", app.api_keys.len(), protocol::KEY_MAX_API, app.key_tab == KeyTab::Api),
        Span::styled("   ‹h  Tab  l›", Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(seg_line), rows[0]);

    // Contextual hint under the segments
    let hint = match app.key_tab {
        KeyTab::Ssh => "  a generate on-device keypair · c show authorized_keys line · import via CLI `key import`",
        KeyTab::Otp => "  a add entry (name + base32 secret) · o fetch code (FP-gated) · bulk import via CLI `key import-otp`",
        KeyTab::Api => "  a add entry (name + value) · s show value (FP-gated)",
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(DIM))),
        rows[1],
    );

    // List
    let list_area = rows[2];
    let visible_rows = list_area.height as usize;
    let cur = app.key_cursor;
    let total = app.current_key_len();

    let start = if total <= visible_rows || cur < visible_rows / 2 {
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
        lines.push(Line::from(Span::styled(hint, Style::default().fg(DIM))));
    } else {
        for idx_in_list in start..end {
            let is_sel = idx_in_list == cur;
            let marker = if is_sel { " ▶ " } else { "   " };
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
                        format!("{}[{:>2}] {:<18}  {}", marker, r.index, truncate(&r.name, 18), fp),
                        row_style,
                    ))
                }
                KeyTab::Otp => {
                    let r = &app.otp_keys[idx_in_list];
                    let svc = if r.service.is_empty() { "-" } else { &r.service };
                    Line::from(Span::styled(
                        format!(
                            "{}[{:>2}] {:<30}  {}",
                            marker,
                            r.index,
                            truncate(&r.name, 30),
                            svc
                        ),
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
}

// ── PAM tab ──────────────────────────────────────────────────

fn draw_pam(f: &mut Frame, app: &App, area: Rect) {
    let block = content_block(" PAM services ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  install/remove needs admin auth — pkexec will prompt.",
        Style::default().fg(DIM),
    )));
    lines.push(Line::from(""));

    for (i, svc) in PAM_SERVICES.iter().enumerate() {
        let is_sel = i == app.pam_cursor;
        let installed = app.pam_is_installed(i);
        let (sym, sym_color) = if installed { ("✓", OK) } else { ("✗", DIM) };
        let status = if installed { "installed" } else { "not installed" };
        let marker = if is_sel { " ▶ " } else { "   " };
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
            Span::styled(format!("{:<14}", svc.display), row_style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", sym), Style::default().fg(sym_color)),
            Span::styled(
                format!("{:<14}", status),
                Style::default().fg(if installed { OK } else { DIM }),
            ),
            Span::styled(format!("  /etc/pam.d/{}", svc.service), Style::default().fg(DIM)),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Logs tab ─────────────────────────────────────────────────

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let live = app.log_child.is_some();
    let tail = if !live {
        Span::styled("● stream closed ", Style::default().fg(ERR))
    } else if app.log_scroll == 0 {
        Span::styled(
            "● live ",
            Style::default().fg(OK).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!("⏸ -{} lines ", app.log_scroll),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Logs · ~/.immurok/logs.txt ",
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .title_top(Line::from(tail).right_aligned())
        .border_style(Style::default().fg(PRIMARY));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = inner.height as usize;
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
    f.render_widget(Paragraph::new(lines), inner);
}

/// Color-code log lines: ERROR red, WARN yellow, INFO default.
/// Matches the level tokens in the daemon's tracing-subscriber output.
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

// ── Message + hotkey footer ──────────────────────────────────

fn draw_message(f: &mut Frame, app: &App, area: Rect) {
    // KeyInput repurposes the message line as the text input.
    if app.mode == Mode::KeyInput {
        let label = match &app.key_add_flow {
            Some(flow) => match flow.stage() {
                KeyInputStage::Name => match flow.cat {
                    KeyTab::Ssh => " New SSH key name: ",
                    KeyTab::Otp => " Account name: ",
                    KeyTab::Api => " New API key name: ",
                },
                KeyInputStage::Service => " Service / issuer (optional): ",
                KeyInputStage::Secret => match flow.cat {
                    KeyTab::Otp => " TOTP secret (base32): ",
                    _ => " API key value: ",
                },
            },
            None => " Input: ",
        };
        // Long secrets: keep the tail visible as the buffer outgrows the line.
        let avail = (area.width as usize).saturating_sub(label.len() + 2);
        let shown: String = {
            let n = app.input_buf.chars().count();
            if n > avail {
                let skip = n - avail;
                let mut s = String::from("…");
                s.extend(app.input_buf.chars().skip(skip + 1));
                s
            } else {
                app.input_buf.clone()
            }
        };
        let input = Paragraph::new(Line::from(vec![
            Span::styled(label, Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
            Span::styled(shown, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                "▏",
                Style::default().fg(PRIMARY).add_modifier(Modifier::SLOW_BLINK),
            ),
        ]));
        f.render_widget(input, area);
        return;
    }

    let (sym, style) = match app.message_style {
        MessageStyle::Dim => ("·", Style::default().fg(DIM)),
        MessageStyle::Green => ("✓", Style::default().fg(OK).add_modifier(Modifier::BOLD)),
        MessageStyle::Red => ("✗", Style::default().fg(ERR).add_modifier(Modifier::BOLD)),
        MessageStyle::Yellow => ("…", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
    };
    f.render_widget(
        Paragraph::new(format!(" {} {}", sym, app.message)).style(style),
        area,
    );
}

fn draw_hotkeys(f: &mut Frame, app: &App, area: Rect) {
    // KeyInput occupies the message line with the text input, so surface
    // validation errors here instead — they'd be invisible otherwise.
    if app.mode == Mode::KeyInput && app.message_style == MessageStyle::Red {
        let key_style = Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled("Enter", key_style),
            Span::styled(" confirm  ·  ", Style::default().fg(DIM)),
            Span::styled("Esc", key_style),
            Span::styled(" cancel   ", Style::default().fg(DIM)),
            Span::styled(
                format!("✗ {}", app.message),
                Style::default().fg(ERR).add_modifier(Modifier::BOLD),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let keys: &[(&str, &str)] = match app.mode {
        Mode::DeleteSelect => &[("0-4", "delete slot"), ("Esc", "cancel")],
        Mode::KeyInput => &[("Enter", "confirm"), ("Esc", "cancel")],
        Mode::KeyDeleteConfirm => &[("y/Enter", "confirm"), ("n/Esc", "cancel")],
        Mode::Help | Mode::Normal => match app.tab {
            Tab::Dashboard => &[
                ("p", "pair"),
                ("u", "unpair"),
                ("e", "enroll"),
                ("d", "delete"),
                ("v", "verify"),
                ("i", "info"),
                ("?", "help"),
                ("q", "quit"),
            ],
            Tab::Keys => match app.key_tab {
                KeyTab::Ssh => &[
                    ("Tab", "category"),
                    ("j/k", "move"),
                    ("a", "generate"),
                    ("d", "del"),
                    ("c", "pubkey"),
                    ("r", "refresh"),
                    ("?", "help"),
                    ("q", "quit"),
                ],
                KeyTab::Otp => &[
                    ("Tab", "category"),
                    ("j/k", "move"),
                    ("a", "add"),
                    ("d", "del"),
                    ("o", "code"),
                    ("r", "refresh"),
                    ("?", "help"),
                    ("q", "quit"),
                ],
                KeyTab::Api => &[
                    ("Tab", "category"),
                    ("j/k", "move"),
                    ("a", "add"),
                    ("d", "del"),
                    ("s", "show value"),
                    ("r", "refresh"),
                    ("?", "help"),
                    ("q", "quit"),
                ],
            },
            Tab::Pam => &[
                ("j/k", "move"),
                ("i", "install"),
                ("r", "remove"),
                ("R", "repair"),
                ("?", "help"),
                ("q", "quit"),
            ],
            Tab::Logs => &[
                ("j/k", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Home", "top"),
                ("End", "tail"),
                ("?", "help"),
                ("q", "quit"),
            ],
        },
    };

    let key_style = Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (k, label)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", Style::default().fg(DIM)));
        }
        if !k.is_empty() {
            spans.push(Span::styled(*k, key_style));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(*label, Style::default().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Help overlay ─────────────────────────────────────────────

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 85, area);
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
            format!("  {:<10}", k),
            Style::default().fg(HOTKEY).add_modifier(Modifier::BOLD),
        )
    };
    let section = |t: &'static str| {
        Line::from(Span::styled(
            t,
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
    };

    let lines = vec![
        section("Navigation"),
        Line::from(vec![key("1-4"), Span::raw("Switch tab · Esc back to Dashboard · q quit")]),
        Line::from(""),
        section("Dashboard"),
        Line::from(vec![key("p / u"), Span::raw("Pair (press device button) · unpair / factory reset")]),
        Line::from(vec![key("e"), Span::raw("Enroll fingerprint (lowest empty slot)")]),
        Line::from(vec![key("d / v"), Span::raw("Delete slot (pick 0-4) · verify fingerprint")]),
        Line::from(vec![key("s o k L"), Span::raw("Toggle sudo / polkit / screen / long-press lock")]),
        Line::from(vec![key("n / i"), Span::raw("Cycle unlock sound · device info")]),
        Line::from(vec![key("Esc"), Span::raw("Cancel in-flight enrollment")]),
        Line::from(""),
        section("Keys"),
        Line::from(vec![key("Tab / h l"), Span::raw("Switch SSH / OTP / API category · j k move")]),
        Line::from(vec![key("a / d / r"), Span::raw("Add (SSH generates on-device) · delete · refresh")]),
        Line::from(vec![key("c / o / s"), Span::raw("SSH pubkey · OTP code · API value (FP-gated)")]),
        Line::from(""),
        section("PAM"),
        Line::from(vec![key("i / r / R"), Span::raw("Install · remove · repair all (pkexec prompts)")]),
        Line::from(""),
        section("Logs"),
        Line::from(vec![key("j k"), Span::raw("Scroll · PgUp/PgDn page · Home top · End follow tail")]),
        Line::from(""),
        Line::from(Span::styled("Press ? or Esc to close.", Style::default().fg(DIM))),
    ];

    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ── helpers ─────────────────────────────────────────────────

/// Standard bordered content block used by tab pages.
fn content_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(PRIMARY))
}

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
