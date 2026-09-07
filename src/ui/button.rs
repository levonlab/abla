//! The floating button: a layer-shell surface that stays above every window
//! and never takes keyboard focus, so the caret stays where you were typing.
//!
//! Everything inside is drawn with cairo: the disc, the microphone, the
//! level bars while you talk, the seconds under the button, and the live
//! text panel. Drawing it ourselves keeps it pixel-exact at any size and
//! makes dragging the button around a matter of drawing it somewhere else.

use std::cell::{Cell, RefCell};
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::App;
use crate::audio::{self, Recorder};
use crate::config::{self, COLORS, SIZES};
use crate::{inject, realtime, transcribe};

/// Room around the disc for its shadow.
const PAD: f64 = 14.0;
/// Height of the seconds line under the disc.
const COUNTER_H: f64 = 22.0;
/// Width of the live text panel, shown to the left of the disc.
const BUBBLE_W: f64 = 320.0;
const BUBBLE_LINES: i32 = 3;
const GAP: f64 = 10.0;
/// Pointer travel before a press turns into a drag.
const DRAG_THRESHOLD: f64 = 8.0;
/// How each of the five bars scales the level, tallest in the middle.
const WEIGHTS: [f32; 5] = [0.60, 0.85, 1.00, 0.80, 0.55];

const IDLE: (f64, f64, f64) = (0.137, 0.153, 0.180);
const BORDER: (f64, f64, f64) = (0.227, 0.251, 0.286);
const INK: (f64, f64, f64) = (0.925, 0.933, 0.945);
const AMBER: (f64, f64, f64) = (0.788, 0.541, 0.106);

enum State {
    Idle,
    Recording {
        recorder: Recorder,
        started: Instant,
        live: Option<realtime::Session>,
    },
    Transcribing {
        result: Receiver<anyhow::Result<String>>,
    },
    /// Live mode after you stop: waiting for the server's final text.
    Finishing {
        started: Instant,
        live: realtime::Session,
    },
    /// A tick mark for a moment after the text was pasted.
    Done {
        until: Instant,
    },
}

struct Move {
    /// Where inside the button box the pointer grabbed it.
    grab_x: f64,
    grab_y: f64,
    /// Width of the surface before it grew to cover the screen.
    small_w: i32,
    /// Where the box was, as margins, until the pointer says otherwise.
    right: i32,
    bottom: i32,
    /// Top-left corner of the box in screen coordinates, once known.
    pos: Option<(f64, f64)>,
}

pub struct FloatingButton {
    app: Rc<App>,
    window: gtk::ApplicationWindow,
    area: gtk::DrawingArea,
    popover: gtk::PopoverMenu,
    state: RefCell<State>,
    level: Cell<f32>,
    bars: RefCell<[f32; 5]>,
    spin: Cell<f64>,
    live_text: RefCell<String>,
    moving: RefCell<Option<Move>>,
    moved_at: Cell<Option<Instant>>,
    ticking: Cell<bool>,
}

impl FloatingButton {
    pub fn new(app: Rc<App>) -> Rc<Self> {
        let button = app.config.borrow().button.clone();
        let window = gtk::ApplicationWindow::builder()
            .application(&app.gtk)
            .title("Abla")
            .decorated(false)
            .resizable(false)
            .build();
        window.add_css_class("abla-float");

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_namespace(Some("abla"));
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Right, true);
        window.set_margin(Edge::Bottom, button.margin_bottom);
        window.set_margin(Edge::Right, button.margin_right);

        let area = gtk::DrawingArea::new();
        window.set_child(Some(&area));

        let popover = gtk::PopoverMenu::from_model(Some(&menu()));
        popover.set_parent(&area);
        popover.set_has_arrow(false);

        let this = Rc::new(Self {
            app,
            window: window.clone(),
            area: area.clone(),
            popover,
            state: RefCell::new(State::Idle),
            level: Cell::new(0.0),
            bars: RefCell::new([0.0; 5]),
            spin: Cell::new(0.0),
            live_text: RefCell::new(String::new()),
            moving: RefCell::new(None),
            moved_at: Cell::new(None),
            ticking: Cell::new(false),
        });
        this.resize();

        {
            let me = this.clone();
            area.set_draw_func(move |_, cr, w, h| me.draw(cr, w as f64, h as f64));
        }

        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        {
            let me = this.clone();
            click.connect_released(move |_, _, _, _| {
                let just_moved = me.moved_at.get().is_some_and(|t| t.elapsed() < Duration::from_millis(300));
                if me.moving.borrow().is_none() && !just_moved {
                    me.toggle();
                }
            });
        }
        area.add_controller(click);

        let secondary = gtk::GestureClick::new();
        secondary.set_button(gdk::BUTTON_SECONDARY);
        {
            let me = this.clone();
            secondary.connect_pressed(move |_, _, x, y| {
                me.popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                me.popover.popup();
            });
        }
        area.add_controller(secondary);

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        {
            let me = this.clone();
            drag.connect_drag_update(move |g, dx, dy| me.on_drag(g, dx, dy));
        }
        {
            let me = this.clone();
            drag.connect_drag_end(move |g, dx, dy| me.on_drag_end(g, dx, dy));
        }
        area.add_controller(drag);

        window.present();
        this
    }

    /// Re-reads size, colour and position after the settings changed.
    pub fn apply_config(&self) {
        let button = self.app.config.borrow().button.clone();
        if self.moving.borrow().is_none() {
            self.window.set_margin(Edge::Bottom, button.margin_bottom);
            self.window.set_margin(Edge::Right, button.margin_right);
        }
        self.resize();
        self.area.queue_draw();
    }

    // ------------------------------------------------------------ geometry

    fn disc(&self) -> f64 {
        self.app.config.borrow().button.size as f64
    }

    fn box_w(&self) -> f64 {
        self.disc() + 2.0 * PAD
    }

    fn box_h(&self) -> f64 {
        self.disc() + 2.0 * PAD + COUNTER_H
    }

    fn bubble_visible(&self) -> bool {
        if self.live_text.borrow().is_empty() {
            return false;
        }
        matches!(
            &*self.state.borrow(),
            State::Recording { live: Some(_), .. } | State::Finishing { .. }
        )
    }

    fn resize(&self) {
        if self.moving.borrow().is_some() {
            return;
        }
        let extra = if self.bubble_visible() { BUBBLE_W + GAP } else { 0.0 };
        self.area.set_content_width((self.box_w() + extra).ceil() as i32);
        self.area.set_content_height(self.box_h().ceil() as i32);
    }

    // ------------------------------------------------------------- dragging

    fn on_drag(&self, gesture: &gtk::GestureDrag, dx: f64, dy: f64) {
        let Some((sx, sy)) = gesture.start_point() else { return };
        if self.moving.borrow().is_none() {
            if dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD {
                return;
            }
            if !matches!(&*self.state.borrow(), State::Idle | State::Done { .. }) {
                return;
            }
            // Cover the whole screen with the surface and draw the button
            // wherever the pointer goes. Layer surfaces cannot be moved, but
            // a full-screen one can draw anywhere. Anchoring to all four
            // edges lets the compositor pick the size; the window has to be
            // resizable for GTK to accept it.
            let box_x = self.area.width() as f64 - self.box_w();
            let button = self.app.config.borrow().button.clone();
            *self.moving.borrow_mut() = Some(Move {
                grab_x: sx - box_x,
                grab_y: sy,
                small_w: self.area.width(),
                right: button.margin_right,
                bottom: button.margin_bottom,
                pos: None,
            });
            self.area.set_content_width(1);
            self.area.set_content_height(1);
            self.window.set_resizable(true);
            for edge in [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom] {
                self.window.set_margin(edge, 0);
                self.window.set_anchor(edge, true);
            }
            return;
        }
        // Once the surface covers the screen, start point plus offset is the
        // pointer in screen coordinates. Until it has grown, events still
        // come in the old coordinates and are ignored.
        let mut moving = self.moving.borrow_mut();
        let Some(m) = moving.as_mut() else { return };
        if self.area.width() <= m.small_w {
            return;
        }
        m.pos = Some((sx + dx - m.grab_x, sy + dy - m.grab_y));
        drop(moving);
        self.area.queue_draw();
    }

    fn on_drag_end(&self, _gesture: &gtk::GestureDrag, _dx: f64, _dy: f64) {
        let Some(m) = self.moving.borrow_mut().take() else { return };
        let (screen_w, screen_h) = (self.area.width() as f64, self.area.height() as f64);
        let (bw, bh) = (self.box_w(), self.box_h());
        let (right, bottom) = match m.pos {
            Some((x, y)) if self.area.width() > m.small_w => {
                let x = x.clamp(0.0, (screen_w - bw).max(0.0));
                let y = y.clamp(0.0, (screen_h - bh).max(0.0));
                (
                    (screen_w - (x + bw)).round().max(0.0) as i32,
                    (screen_h - (y + bh)).round().max(0.0) as i32,
                )
            }
            // Released before the surface grew: leave it where it was.
            _ => (m.right, m.bottom),
        };

        for edge in [Edge::Left, Edge::Top] {
            self.window.set_anchor(edge, false);
        }
        self.window.set_margin(Edge::Right, right);
        self.window.set_margin(Edge::Bottom, bottom);
        self.window.set_resizable(false);
        self.resize();
        self.moved_at.set(Some(Instant::now()));
        {
            let mut config = self.app.config.borrow_mut();
            config.button.margin_right = right;
            config.button.margin_bottom = bottom;
            if let Err(err) = config.save() {
                log::warn!("could not save the position: {err:#}");
            }
        }
        self.area.queue_draw();
    }

    // ------------------------------------------------------------- dictation

    pub fn toggle(self: &Rc<Self>) {
        let idle = matches!(&*self.state.borrow(), State::Idle | State::Done { .. });
        let recording = matches!(&*self.state.borrow(), State::Recording { .. });
        if idle {
            self.start_recording();
        } else if recording {
            self.stop_recording();
        }
    }

    fn start_recording(self: &Rc<Self>) {
        let config = self.app.config.borrow().clone();
        if !config.is_ready() {
            self.app.gtk.activate_action("settings", None);
            return;
        }
        let (live_tx, live) = if config.provider.realtime {
            let (tx, rx) = channel();
            (Some(tx), Some(realtime::start(&config.provider, &config.api_key(), rx)))
        } else {
            (None, None)
        };
        match Recorder::start(live_tx) {
            Ok(recorder) => {
                self.live_text.borrow_mut().clear();
                self.level.set(0.0);
                *self.bars.borrow_mut() = [0.0; 5];
                *self.state.borrow_mut() = State::Recording { recorder, started: Instant::now(), live };
                self.start_ticker();
            }
            Err(err) => notify(&format!("{err:#}")),
        }
        self.resize();
        self.area.queue_draw();
    }

    fn stop_recording(self: &Rc<Self>) {
        let previous = std::mem::replace(&mut *self.state.borrow_mut(), State::Idle);
        let State::Recording { recorder, live, .. } = previous else { return };
        // Dropping the recorder's sender tells a live session the audio ended.
        let pcm = recorder.stop();
        let seconds = pcm.len() as f32 / audio::BYTES_PER_SECOND as f32;
        if seconds < 0.4 {
            notify("Nothing was recorded.");
        } else if let Some(live) = live {
            *self.state.borrow_mut() = State::Finishing { started: Instant::now(), live };
        } else {
            let config = self.app.config.borrow().clone();
            let key = config.api_key();
            let (tx, rx) = channel();
            std::thread::Builder::new()
                .name("abla-transcribe".into())
                .spawn(move || {
                    let wav = audio::wav(&pcm, audio::SAMPLE_RATE);
                    let _ = tx.send(transcribe::batch(&config.provider, &key, wav));
                })
                .expect("could not start the transcription thread");
            *self.state.borrow_mut() = State::Transcribing { result: rx };
        }
        self.start_ticker();
        self.resize();
        self.area.queue_draw();
    }

    fn start_ticker(self: &Rc<Self>) {
        if self.ticking.get() {
            return;
        }
        self.ticking.set(true);
        let me = self.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if me.tick() {
                glib::ControlFlow::Continue
            } else {
                me.ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// One animation frame. Returns false once there is nothing left to do.
    fn tick(self: &Rc<Self>) -> bool {
        let mut deliver: Option<String> = None;
        let mut error: Option<String> = None;
        let mut warning: Option<String> = None;
        let mut finished = false;
        {
            let mut state = self.state.borrow_mut();
            match &mut *state {
                State::Recording { recorder, started, live } => {
                    let target = recorder.level();
                    let current = self.level.get();
                    let mut next = current + (target - current) * if target > current { 0.55 } else { 0.18 };
                    if next < 0.01 {
                        next = 0.0;
                    }
                    self.level.set(next);
                    let t = started.elapsed().as_secs_f32();
                    let mut bars = self.bars.borrow_mut();
                    for (i, bar) in bars.iter_mut().enumerate() {
                        let wobble = 0.82 + 0.18 * (t * (7.0 + i as f32 * 1.7) + i as f32).sin();
                        *bar = (next * WEIGHTS[i] * wobble).clamp(0.0, 1.0);
                    }
                    if let Some(session) = live {
                        let mut drop_live = false;
                        loop {
                            match session.events.try_recv() {
                                Ok(realtime::Event::Delta(text)) => self.live_text.borrow_mut().push_str(&text),
                                Ok(realtime::Event::Error(e)) => {
                                    warning = Some(format!("Live transcription stopped: {e}"));
                                    drop_live = true;
                                    break;
                                }
                                Ok(realtime::Event::Done(_)) | Ok(realtime::Event::Closed) => {
                                    warning = Some("The live session ended early.".into());
                                    drop_live = true;
                                    break;
                                }
                                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                            }
                        }
                        if drop_live {
                            *live = None;
                        }
                    }
                }
                State::Transcribing { result } => {
                    self.spin.set(self.spin.get() + 0.11);
                    match result.try_recv() {
                        Ok(Ok(text)) => deliver = Some(text),
                        Ok(Err(e)) => error = Some(format!("{e:#}")),
                        Err(TryRecvError::Disconnected) => error = Some("the transcription stopped unexpectedly".into()),
                        Err(TryRecvError::Empty) => {}
                    }
                }
                State::Finishing { started, live } => {
                    self.spin.set(self.spin.get() + 0.11);
                    loop {
                        match live.events.try_recv() {
                            Ok(realtime::Event::Delta(text)) => self.live_text.borrow_mut().push_str(&text),
                            Ok(realtime::Event::Done(text)) => {
                                let so_far = self.live_text.borrow().clone();
                                deliver = Some(if text.trim().is_empty() { so_far } else { text });
                                break;
                            }
                            Ok(realtime::Event::Error(e)) => {
                                let so_far = self.live_text.borrow().clone();
                                if so_far.trim().is_empty() {
                                    error = Some(e);
                                } else {
                                    deliver = Some(so_far);
                                }
                                break;
                            }
                            Ok(realtime::Event::Closed) | Err(TryRecvError::Disconnected) => {
                                let so_far = self.live_text.borrow().clone();
                                if so_far.trim().is_empty() {
                                    error = Some("the live session closed without a transcript".into());
                                } else {
                                    deliver = Some(so_far);
                                }
                                break;
                            }
                            Err(TryRecvError::Empty) => break,
                        }
                    }
                    if deliver.is_none() && error.is_none() && started.elapsed() > Duration::from_secs(30) {
                        let so_far = self.live_text.borrow().clone();
                        if so_far.trim().is_empty() {
                            error = Some("the live session did not finish in time".into());
                        } else {
                            deliver = Some(so_far);
                        }
                    }
                }
                State::Done { until } => {
                    if Instant::now() >= *until {
                        *state = State::Idle;
                        finished = true;
                    }
                }
                State::Idle => finished = true,
            }
        }
        if let Some(text) = deliver {
            *self.state.borrow_mut() = State::Done { until: Instant::now() + Duration::from_millis(1500) };
            self.live_text.borrow_mut().clear();
            self.deliver(text);
        }
        if let Some(message) = error {
            *self.state.borrow_mut() = State::Idle;
            self.live_text.borrow_mut().clear();
            notify(&format!("Transcription failed: {message}"));
            finished = true;
        }
        if let Some(message) = warning {
            notify(&message);
        }
        self.resize();
        self.area.queue_draw();
        !finished
    }

    /// Puts the text on the clipboard, presses Ctrl+V, then restores what the
    /// clipboard held before. Without ydotool the text stays on the clipboard.
    fn deliver(self: &Rc<Self>, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            notify("Nothing was heard.");
            return;
        }
        let clipboard = WidgetExt::display(&self.window).clipboard();
        let cb = clipboard.clone();
        clipboard.read_text_async(None::<&gio::Cancellable>, move |previous| {
            let previous = previous
                .ok()
                .flatten()
                .map(|g| g.to_string())
                .filter(|p| !p.is_empty() && *p != text);
            cb.set_text(&text);
            let cb = cb.clone();
            glib::timeout_add_local_once(Duration::from_millis(150), move || match inject::ctrl_v() {
                Ok(()) => {
                    if let Some(previous) = previous {
                        glib::timeout_add_local_once(Duration::from_millis(900), move || cb.set_text(&previous));
                    }
                }
                Err(err) => {
                    log::warn!("{err:#}");
                    notify("Copied to the clipboard. Press Ctrl+V to paste.\nRun `abla doctor` to set up automatic pasting.");
                }
            });
        });
    }

    // -------------------------------------------------------------- drawing

    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        let s = self.disc();
        let (bw, bh) = (self.box_w(), self.box_h());
        let moving = self.moving.borrow();
        let (ox, oy) = match moving.as_ref() {
            Some(Move { pos: Some(p), .. }) => *p,
            // Growing to full screen but the pointer has not spoken yet: keep
            // the box where it was, so nothing flashes in a corner.
            Some(m) if w > m.small_w as f64 + 0.5 => (w - m.right as f64 - bw, h - m.bottom as f64 - bh),
            _ => (w - bw, 0.0),
        };
        let cx = ox + bw / 2.0;
        let cy = oy + PAD + s / 2.0;

        let state = self.state.borrow();
        let recording = matches!(&*state, State::Recording { .. });
        let busy = matches!(&*state, State::Transcribing { .. } | State::Finishing { .. });
        let done = matches!(&*state, State::Done { .. });
        let (ar, ag, ab) = config::rgb(&self.app.config.borrow().button.color);

        if moving.is_none() && self.bubble_visible() {
            self.draw_bubble(cr, ox - GAP - BUBBLE_W, oy + PAD + s, &self.live_text.borrow());
        }

        // Shadow: a few soft rings instead of a blur.
        for i in 0..4 {
            cr.arc(cx, cy + 3.0, s / 2.0 + i as f64 * 2.0, 0.0, TAU);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.11 - i as f64 * 0.025);
            let _ = cr.fill();
        }

        let (r, g, b) = if recording {
            (ar * 0.86, ag * 0.86, ab * 0.86)
        } else if busy {
            (AMBER.0 * 0.78, AMBER.1 * 0.78, AMBER.2 * 0.78)
        } else {
            IDLE
        };
        cr.arc(cx, cy, s / 2.0, 0.0, TAU);
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill_preserve();
        if recording {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.22);
        } else {
            cr.set_source_rgb(BORDER.0, BORDER.1, BORDER.2);
        }
        cr.set_line_width(1.0);
        let _ = cr.stroke();

        if recording {
            draw_bars(cr, cx, cy, s, &self.bars.borrow());
        } else {
            draw_mic(cr, cx, cy, s, if busy { 0.5 } else { 1.0 });
        }
        if busy {
            let a = self.spin.get();
            cr.set_line_width(2.5);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.18);
            cr.arc(cx, cy, s / 2.0 - 4.5, 0.0, TAU);
            let _ = cr.stroke();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
            cr.arc(cx, cy, s / 2.0 - 4.5, a, a + FRAC_PI_2);
            let _ = cr.stroke();
        }

        if moving.is_none() {
            let label = match &*state {
                State::Recording { started, .. } => {
                    let secs = started.elapsed().as_secs();
                    format!("{}:{:02}", secs / 60, secs % 60)
                }
                State::Transcribing { .. } | State::Finishing { .. } => "···".to_string(),
                State::Done { .. } if done => "✓".to_string(),
                _ => String::new(),
            };
            if !label.is_empty() {
                draw_counter(cr, &label, cx, oy + bh - COUNTER_H + 2.0, recording, (ar, ag, ab));
            }
        }
    }

    fn draw_bubble(&self, cr: &cairo::Context, x: f64, bottom: f64, text: &str) {
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 11")));
        layout.set_width(((BUBBLE_W - 28.0) * pango::SCALE as f64) as i32);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_ellipsize(pango::EllipsizeMode::Start);
        layout.set_height(-BUBBLE_LINES);
        layout.set_text(text);
        let (_, text_h) = layout.pixel_size();
        let h = text_h as f64 + 24.0;
        let y = (bottom - h).max(0.0);
        rounded_rect(cr, x, y, BUBBLE_W, h, 12.0);
        cr.set_source_rgba(0.11, 0.12, 0.14, 0.94);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.10);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        cr.set_source_rgb(INK.0, INK.1, INK.2);
        cr.move_to(x + 14.0, y + 12.0);
        pangocairo::functions::show_layout(cr, &layout);
    }
}

fn draw_mic(cr: &cairo::Context, cx: f64, cy: f64, s: f64, alpha: f64) {
    let k = s * 0.46 / 24.0;
    let _ = cr.save();
    cr.translate(cx - 12.0 * k, cy - 12.0 * k);
    cr.scale(k, k);
    cr.set_line_width(1.8);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_source_rgba(INK.0, INK.1, INK.2, alpha);
    rounded_rect(cr, 9.0, 2.0, 6.0, 11.0, 3.0);
    let _ = cr.stroke();
    cr.new_sub_path();
    cr.arc(12.0, 10.5, 7.0, 0.0, PI);
    let _ = cr.stroke();
    cr.move_to(12.0, 17.5);
    cr.line_to(12.0, 21.0);
    let _ = cr.stroke();
    cr.move_to(8.5, 21.0);
    cr.line_to(15.5, 21.0);
    let _ = cr.stroke();
    let _ = cr.restore();
}

fn draw_bars(cr: &cairo::Context, cx: f64, cy: f64, s: f64, bars: &[f32; 5]) {
    let height = s * 0.46;
    let width = s * 0.05;
    let gap = s * 0.045;
    let total = 5.0 * width + 4.0 * gap;
    let x0 = cx - total / 2.0 + width / 2.0;
    cr.set_line_width(width);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    for (i, v) in bars.iter().enumerate() {
        let h = (0.14 + 0.86 * (*v as f64).clamp(0.0, 1.0)) * height;
        let half = (h / 2.0 - width / 2.0).max(0.0);
        let x = x0 + i as f64 * (width + gap);
        cr.move_to(x, cy - half);
        cr.line_to(x, cy + half);
        let _ = cr.stroke();
    }
}

fn draw_counter(cr: &cairo::Context, text: &str, cx: f64, y: f64, recording: bool, accent: (f64, f64, f64)) {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(&pango::FontDescription::from_string("Monospace Bold 10")));
    layout.set_text(text);
    let (tw, _) = layout.pixel_size();
    let x = cx - tw as f64 / 2.0;
    // A dark halo first, so the digits read on any wallpaper.
    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (0.0, 2.0)] {
        cr.move_to(x + dx, y + dy);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        pangocairo::functions::show_layout(cr, &layout);
    }
    cr.move_to(x, y);
    if recording {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    let _ = accent;
    pangocairo::functions::show_layout(cr, &layout);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

fn menu() -> gio::Menu {
    let size = gio::Menu::new();
    for s in SIZES {
        let item = gio::MenuItem::new(Some(&format!("{s} px")), None);
        item.set_action_and_target_value(Some("app.size"), Some(&s.to_string().to_variant()));
        size.append_item(&item);
    }
    let color = gio::Menu::new();
    for (name, hex) in COLORS {
        let item = gio::MenuItem::new(Some(name), None);
        item.set_action_and_target_value(Some("app.color"), Some(&hex.to_variant()));
        color.append_item(&item);
    }
    let look = gio::Menu::new();
    look.append_submenu(Some("Size"), &size);
    look.append_submenu(Some("Recording color"), &color);

    let system = gio::Menu::new();
    system.append(Some("Start at login"), Some("app.autostart"));
    system.append(Some("Settings…"), Some("app.settings"));

    let quit = gio::Menu::new();
    quit.append(Some("Quit"), Some("app.quit"));

    let menu = gio::Menu::new();
    menu.append_section(None, &look);
    menu.append_section(None, &system);
    menu.append_section(None, &quit);
    menu
}

/// A desktop notification, off the main thread so a slow D-Bus never
/// freezes the button.
pub fn notify(message: &str) {
    let body = message.to_string();
    log::info!("{body}");
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .summary("Abla")
            .body(&body)
            .icon("audio-input-microphone")
            .timeout(notify_rust::Timeout::Milliseconds(6000))
            .show();
    });
}
