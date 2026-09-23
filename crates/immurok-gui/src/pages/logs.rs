//! Logs page (spec §7): live tail of the daemon log over `SUBSCRIBE:LOG`,
//! level colouring, pause-on-scroll. A reader thread blocks on the socket
//! and forwards lines over a channel; the main loop appends them in batches
//! and keeps at most `LOG_CAP` lines in the buffer.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::open_log_stream;

use super::run_blocking;

pub const LOG_CAP: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Dim,
    Plain,
}

/// Same substring rules as the TUI's `log_line_style`; the daemon's
/// "… N log lines dropped" marker counts as a warning.
pub fn classify(line: &str) -> Level {
    if line.contains(" ERROR ") || line.contains("ERROR:") || line.contains("[ERROR]") || line.contains(" panicked") {
        Level::Error
    } else if line.contains(" WARN ") || line.contains("WARN:") || line.contains("[WARN]") || line.contains("log lines dropped") {
        Level::Warn
    } else if line.contains(" DEBUG ") || line.contains(" TRACE ") {
        Level::Dim
    } else {
        Level::Plain
    }
}

pub fn lines_to_drop(line_count: usize, cap: usize) -> usize {
    line_count.saturating_sub(cap)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Connecting,
    Live,
    Paused(usize),
    Closed,
}

pub struct LogsPage {
    root: gtk::Box,
    badge: gtk::Label,
    jump: gtk::Button,
    reconnect: gtk::Button,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    /// Right-gravity mark pinned to the end of the buffer: scrolling to a mark
    /// survives line heights that are not validated yet, which `scroll_to_iter`
    /// does not (it would leave the view short of the bottom after a burst and
    /// flip the page to Paused without any user scroll).
    end_mark: gtk::TextMark,
    scroller: gtk::ScrolledWindow,
    state: Cell<StreamState>,
    /// Whether new lines should scroll the view. Owned by us instead of being
    /// re-sampled from the adjustment on every batch: only the `value_changed`
    /// handler (i.e. an actual scroll) and "Jump to latest" change it.
    follow: Cell<bool>,
    stream: RefCell<Option<UnixStream>>,
    /// Bumped by every `connect()`; a consumer future whose generation is stale
    /// must not append or touch the state (ledger P4-R7).
    generation: Cell<u64>,
    loaded_once: Cell<bool>,
}

impl LogsPage {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let badge = gtk::Label::new(Some("● Connecting…"));
        badge.set_hexpand(true);
        badge.set_xalign(0.0);
        let jump = gtk::Button::builder().label("Jump to latest").sensitive(false).build();
        let reconnect = gtk::Button::builder().label("Reconnect").sensitive(false).build();
        header.append(&badge);
        header.append(&jump);
        header.append(&reconnect);

        let buffer = gtk::TextBuffer::new(None);
        let table = buffer.tag_table();
        let error = gtk::TextTag::builder().name("error").foreground("#e01b24").weight(700).build();
        let warn = gtk::TextTag::builder().name("warn").foreground("#e5a50a").build();
        let dim = gtk::TextTag::builder().name("dim").foreground("#77767b").build();
        table.add(&error);
        table.add(&warn);
        table.add(&dim);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::None)
            .left_margin(6)
            .right_margin(6)
            .build();
        let scroller = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();
        // `left_gravity = false` → text inserted at the mark lands before it,
        // so the mark stays at the very end of the buffer for its whole life.
        let end_mark = buffer.create_mark(None, &buffer.end_iter(), false);

        root.append(&header);
        root.append(&scroller);

        let this = Rc::new(Self {
            root,
            badge,
            jump,
            reconnect,
            view,
            buffer,
            end_mark,
            scroller,
            state: Cell::new(StreamState::Connecting),
            follow: Cell::new(true),
            stream: RefCell::new(None),
            generation: Cell::new(0),
            loaded_once: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.jump.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.jump_to_end();
            }
        });
        let weak = Rc::downgrade(&this);
        this.reconnect.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.connect();
            }
        });
        // Scrolling up pauses; scrolling back to the bottom resumes. This is
        // the only place that clears `follow`, so a burst of appends can no
        // longer be mistaken for the user having scrolled away. A programmatic
        // scroll that lands at the bottom takes the first branch and ends Live,
        // which is exactly what "Jump to latest" and auto-follow want.
        let weak = Rc::downgrade(&this);
        this.scroller.vadjustment().connect_value_changed(move |_| {
            let Some(p) = weak.upgrade() else { return };
            if p.at_bottom() {
                p.follow.set(true);
                if let StreamState::Paused(_) = p.state.get() {
                    p.set_state(StreamState::Live);
                }
            } else if p.follow.replace(false) {
                // Left the bottom while following: pause from a zero count.
                // If we were already Paused, `follow` was false and the count
                // accumulated by `append` is kept untouched.
                if let StreamState::Live = p.state.get() {
                    p.set_state(StreamState::Paused(0));
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.loaded_once.replace(true) {
                    p.connect();
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Close the socket so the reader thread ends. Safe to call twice.
    pub fn shutdown(&self) {
        if let Some(s) = self.stream.borrow_mut().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    fn set_state(&self, s: StreamState) {
        self.state.set(s);
        let text = match s {
            StreamState::Connecting => "● Connecting…".to_string(),
            StreamState::Live => "● Live".to_string(),
            StreamState::Paused(n) => format!("⏸ Paused ({n} new lines)"),
            StreamState::Closed => "● Stream closed".to_string(),
        };
        self.badge.set_text(&text);
        self.jump.set_sensitive(matches!(s, StreamState::Paused(_)));
        self.reconnect.set_sensitive(matches!(s, StreamState::Closed));
    }

    fn at_bottom(&self) -> bool {
        let adj = self.scroller.vadjustment();
        adj.value() + adj.page_size() >= adj.upper() - 1.0
    }

    fn jump_to_end(&self) {
        self.follow.set(true);
        self.view.scroll_to_mark(&self.end_mark, 0.0, true, 0.0, 1.0);
        self.set_state(StreamState::Live);
    }

    fn connect(self: &Rc<Self>) {
        // Claim a generation before tearing the old socket down: everything the
        // previous attempt still has in flight (an `open_log_stream` that has
        // not returned, a consumer future draining its channel) compares against
        // this number and bows out instead of appending or setting Closed.
        let my_gen = self.generation.get() + 1;
        self.generation.set(my_gen);
        self.shutdown();
        self.set_state(StreamState::Connecting);
        self.follow.set(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let opened = run_blocking(open_log_stream).await;
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() != my_gen {
                return; // superseded while opening; dropping `opened` closes it
            }
            let stream = match opened {
                Some(Ok(s)) => s,
                _ => {
                    this.set_state(StreamState::Closed);
                    return;
                }
            };
            let reader = match stream.try_clone() {
                Ok(r) => r,
                Err(_) => {
                    this.set_state(StreamState::Closed);
                    return;
                }
            };
            *this.stream.borrow_mut() = Some(stream);
            this.set_state(StreamState::Live);

            let (tx, rx) = async_channel::unbounded::<String>();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines() {
                    match line {
                        Ok(l) => {
                            if tx.send_blocking(l).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // Dropping `tx` closes the channel → the UI shows "Stream closed".
            });

            let weak = Rc::downgrade(&this);
            drop(this);
            glib::spawn_future_local(async move {
                while let Ok(first) = rx.recv().await {
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let Some(this) = weak.upgrade() else { return };
                    if this.generation.get() != my_gen {
                        return; // a newer stream owns the view now
                    }
                    this.append(&batch);
                }
                if let Some(this) = weak.upgrade() {
                    if this.generation.get() == my_gen {
                        this.stream.borrow_mut().take();
                        this.set_state(StreamState::Closed);
                    }
                }
            });
        });
    }

    fn append(&self, lines: &[String]) {
        let follow = self.follow.get();
        for line in lines {
            let mut end = self.buffer.end_iter();
            let start_off = end.offset();
            self.buffer.insert(&mut end, line);
            self.buffer.insert(&mut end, "\n");
            let tag = match classify(line) {
                Level::Error => Some("error"),
                Level::Warn => Some("warn"),
                Level::Dim => Some("dim"),
                Level::Plain => None,
            };
            if let Some(t) = tag {
                let s = self.buffer.iter_at_offset(start_off);
                let e = self.buffer.end_iter();
                self.buffer.apply_tag_by_name(t, &s, &e);
            }
        }
        let drop_n = lines_to_drop(self.buffer.line_count() as usize, LOG_CAP + 1);
        if drop_n > 0 {
            let mut a = self.buffer.start_iter();
            let mut b = self.buffer.iter_at_line(drop_n as i32).unwrap_or_else(|| self.buffer.start_iter());
            self.buffer.delete(&mut a, &mut b);
        }
        if follow {
            self.view.scroll_to_mark(&self.end_mark, 0.0, true, 0.0, 1.0);
        } else {
            let n = match self.state.get() {
                StreamState::Paused(n) => n,
                _ => 0,
            };
            self.set_state(StreamState::Paused(n + lines.len()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_the_tui() {
        assert_eq!(classify("2026-09-18 ERROR ble: gone"), Level::Error);
        assert_eq!(classify("ERROR: x"), Level::Error);
        assert_eq!(classify("[ERROR] x"), Level::Error);
        assert_eq!(classify("thread 'main' panicked at"), Level::Error);
        assert_eq!(classify("2026 WARN  slow"), Level::Warn);
        assert_eq!(classify("[WARN] y"), Level::Warn);
        assert_eq!(classify("… 12 log lines dropped (viewer too slow)"), Level::Warn);
        assert_eq!(classify("2026 DEBUG poll"), Level::Dim);
        assert_eq!(classify("2026 TRACE poll"), Level::Dim);
        assert_eq!(classify("2026 INFO ok"), Level::Plain);
    }

    #[test]
    fn ring_trim() {
        assert_eq!(lines_to_drop(999, 1000), 0);
        assert_eq!(lines_to_drop(1000, 1000), 0);
        assert_eq!(lines_to_drop(1003, 1000), 3);
        // `append` always ends the buffer with a newline, so GTK reports one
        // extra (empty) trailing line: the live call passes `LOG_CAP + 1` as
        // the cap, which keeps exactly LOG_CAP real lines.
        assert_eq!(lines_to_drop(LOG_CAP + 1 + 3, LOG_CAP + 1), 3);
    }
}
