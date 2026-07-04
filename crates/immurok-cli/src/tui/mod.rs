//! TUI panel — interactive ratatui-based terminal UI.

pub mod app;
pub mod widgets;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;

use app::{App, KeyTab, PamRequest, Tab, PAM_SERVICES};

/// Run the TUI.
pub fn run() -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.refresh();

    let tick_rate = Duration::from_millis(200);
    let poll_interval = Duration::from_secs(2);
    let mut last_poll = Instant::now();

    let mut pam_request: Option<PamRequest> = None;

    loop {
        terminal.draw(|f| widgets::draw(f, &app))?;

        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                // Ctrl-C → quit
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }

                match app.mode {
                    app::Mode::Normal => match key.code {
                        // ── Global keys ──────────────────────
                        KeyCode::Char('q') => break,
                        KeyCode::Char('?') => app.toggle_help(),
                        KeyCode::Char('1') => app.set_tab(Tab::Dashboard),
                        KeyCode::Char('2') => app.set_tab(Tab::Keys),
                        KeyCode::Char('3') => app.set_tab(Tab::Pam),
                        KeyCode::Char('4') => app.set_tab(Tab::Logs),

                        // ── Per-tab keys ─────────────────────
                        code => match app.tab {
                            Tab::Dashboard => match code {
                                KeyCode::Char('p') => app.action_pair(),
                                KeyCode::Char('u') => app.action_unpair(),
                                // Enrolls into the lowest empty slot —
                                // no manual slot picking.
                                KeyCode::Char('e') | KeyCode::Char('E') => app.auto_enroll(),
                                KeyCode::Char('d') => app.enter_delete_select(),
                                KeyCode::Char('v') => app.action_verify(),
                                KeyCode::Char('s') => app.action_toggle_sudo(),
                                KeyCode::Char('o') => app.action_toggle_polkit(),
                                KeyCode::Char('k') => app.action_toggle_screen(),
                                KeyCode::Char('L') => app.action_toggle_lock(),
                                KeyCode::Char('n') => app.action_cycle_sound(),
                                KeyCode::Char('i') => app.action_info(),
                                KeyCode::Esc => {
                                    // Cancel in-flight enrollment if any
                                    if app.enroll_active {
                                        app.action_enroll_cancel();
                                    }
                                }
                                _ => {}
                            },

                            Tab::Keys => match code {
                                KeyCode::Esc => app.set_tab(Tab::Dashboard),
                                KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                                    app.keys_next_tab()
                                }
                                KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                                    app.keys_set_tab(app.key_tab.prev())
                                }
                                KeyCode::Char('S') => app.keys_set_tab(KeyTab::Ssh),
                                KeyCode::Char('O') => app.keys_set_tab(KeyTab::Otp),
                                KeyCode::Char('A') => app.keys_set_tab(KeyTab::Api),
                                KeyCode::Up | KeyCode::Char('k') => app.keys_cursor_up(),
                                KeyCode::Down | KeyCode::Char('j') => app.keys_cursor_down(),
                                KeyCode::Char('r') => {
                                    app.refresh_keys();
                                    app.set_msg_dim("Key cache reloaded.");
                                }
                                // `a` add (per-category flow); `g` kept as a
                                // muscle-memory alias from the old SSH-only UI.
                                KeyCode::Char('a') | KeyCode::Char('g') => app.enter_key_add(),
                                KeyCode::Char('d') => app.enter_key_delete_confirm(),
                                KeyCode::Char('o') => app.action_key_otp(),
                                KeyCode::Char('c') => app.action_key_show_pubkey(),
                                KeyCode::Char('s') => app.action_key_show_api(),
                                _ => {}
                            },

                            Tab::Pam => match code {
                                KeyCode::Esc => app.set_tab(Tab::Dashboard),
                                KeyCode::Up | KeyCode::Char('k') => app.pam_cursor_up(),
                                KeyCode::Down | KeyCode::Char('j') => app.pam_cursor_down(),
                                KeyCode::Char('i') => {
                                    pam_request = app.request_pam_action(true);
                                }
                                KeyCode::Char('r') => {
                                    pam_request = app.request_pam_action(false);
                                }
                                KeyCode::Char('R') => {
                                    if let Some(req) = app.request_pam_repair() {
                                        pam_request = Some(req);
                                    } else {
                                        app.set_msg(
                                            "PAM already configured — nothing to repair.",
                                            app::MessageStyle::Green,
                                        );
                                    }
                                }
                                _ => {}
                            },

                            Tab::Logs => match code {
                                KeyCode::Esc => app.set_tab(Tab::Dashboard),
                                KeyCode::Up | KeyCode::Char('k') => app.log_scroll_up(),
                                KeyCode::Down | KeyCode::Char('j') => app.log_scroll_down(),
                                KeyCode::PageUp => app.log_page_up(),
                                KeyCode::PageDown => app.log_page_down(),
                                KeyCode::Home => app.log_jump_top(),
                                KeyCode::End => app.log_jump_bottom(),
                                _ => {}
                            },
                        },
                    },

                    app::Mode::Help => match key.code {
                        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                            app.toggle_help();
                        }
                        _ => {}
                    },

                    app::Mode::DeleteSelect => match key.code {
                        KeyCode::Esc => app.cancel_select(),
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            let slot = c as u8 - b'0';
                            if slot < immurok_common::protocol::MAX_FINGERPRINT_SLOTS {
                                app.action_delete(slot);
                            }
                        }
                        _ => {}
                    },

                    app::Mode::KeyInput => match key.code {
                        KeyCode::Esc => app.input_cancel(),
                        KeyCode::Enter => app.input_submit_key(),
                        KeyCode::Backspace => app.input_pop_char(),
                        KeyCode::Char(c) => app.input_push_char(c),
                        _ => {}
                    },

                    app::Mode::KeyDeleteConfirm => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            app.confirm_key_delete();
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.cancel_key_delete();
                        }
                        _ => {}
                    },
                }
            }
        }

        // Run a PAM action if requested (needs to leave alt-screen for pkexec)
        if let Some(req) = pam_request.take() {
            disable_raw_mode()?;
            io::stdout().execute(LeaveAlternateScreen)?;
            let ok = run_pam_helper(req.action, &req.services);
            enable_raw_mode()?;
            io::stdout().execute(EnterAlternateScreen)?;
            terminal.clear()?;
            app.after_pam_action(&req, ok);
            app.refresh();
        }

        let needs_refresh = app.drain_actions();
        if needs_refresh || (last_poll.elapsed() >= poll_interval && !app.busy) {
            app.refresh();
            last_poll = Instant::now();
        }
    }

    // Cancel any in-progress enrollment before exit
    if app.busy {
        let _ = super::socket_client::DaemonClient::connect()
            .and_then(|mut c| c.send("FP:ENROLL_CANCEL"));
    }

    // Reap journalctl child (if Logs panel was open).
    app.shutdown();

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

/// Locate `immurok-pam-helper` next to this binary or in PATH.
fn find_pam_helper() -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("immurok-pam-helper");
            if candidate.exists() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    if let Ok(output) = std::process::Command::new("which")
        .arg("immurok-pam-helper")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    None
}

/// Run the PAM helper via pkexec for one or more services. Returns true on success.
/// Success = pkexec exit 0 AND no ERROR: lines in helper stdout.
fn run_pam_helper(action: &str, services: &[&str]) -> bool {
    let helper = match find_pam_helper() {
        Some(h) => h,
        None => {
            eprintln!("Error: immurok-pam-helper not found in PATH or next to this binary.");
            return false;
        }
    };

    let svc_list = services.join(" ");
    println!("Running: pkexec {} {} {}", helper, action, svc_list);

    let mut args: Vec<&str> = vec![&helper, action];
    args.extend_from_slice(services);

    match std::process::Command::new("pkexec")
        .args(&args)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // 把 helper stdout 原样打印（TUI 此时已离开 alt-screen，能显示）
            print!("{}", stdout);
            if !output.status.success() {
                // pkexec 自身失败（如用户取消授权 exit 126/127），helper 根本没跑
                eprintln!(
                    "\x1b[31mPAM helper failed (exit code: {})\x1b[0m",
                    output.status.code().unwrap_or(-1)
                );
                return false;
            }
            if crate::commands::pam::helper_output_has_error(&stdout) {
                eprintln!(
                    "\x1b[31mPAM {} for '{}' failed (see ERROR lines above).\x1b[0m",
                    if action == "add" { "install" } else { "remove" },
                    svc_list
                );
                return false;
            }
            println!(
                "\x1b[32mPAM {} for '{}' succeeded.\x1b[0m",
                if action == "add" { "install" } else { "remove" },
                svc_list
            );
            true
        }
        Err(e) => {
            eprintln!("Failed to run pkexec: {}", e);
            false
        }
    }
}

// Silence unused-import warning if PAM_SERVICES isn't referenced here.
const _: usize = PAM_SERVICES.len();
