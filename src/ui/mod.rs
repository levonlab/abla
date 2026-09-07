//! The GTK application: one floating button, a settings window, and the
//! actions the right-click menu and `abla toggle` talk to.

pub mod button;
pub mod setup;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::config::Config;
use crate::desktop;
use button::FloatingButton;

pub const APP_ID: &str = "io.github.levongabrielyan.abla";

const CSS: &str = "
window.abla-float { background: transparent; }
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Button,
    Settings,
}

pub struct App {
    pub gtk: gtk::Application,
    pub config: RefCell<Config>,
    button: RefCell<Option<Rc<FloatingButton>>>,
}

pub fn run(start: Start) -> Result<i32> {
    let gtk_app = gtk::Application::builder().application_id(APP_ID).build();
    gtk_app.register(None::<&gio::Cancellable>)?;
    if gtk_app.is_remote() {
        // Another abla owns the name: hand it the request and leave.
        match start {
            Start::Settings => gtk_app.activate_action("settings", None),
            Start::Button => gtk_app.activate_action("show", None),
        }
        flush(&gtk_app);
        return Ok(0);
    }

    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let app = Rc::new(App {
        gtk: gtk_app.clone(),
        config: RefCell::new(Config::load_or_default()),
        button: RefCell::new(None),
    });
    register_actions(&app);

    let a = app.clone();
    gtk_app.connect_activate(move |_| match start {
        Start::Settings => a.open_settings(),
        Start::Button => a.ensure_button(),
    });
    Ok(i32::from(gtk_app.run_with_args::<String>(&[])))
}

/// `abla toggle`: asks the running instance to start or stop dictation.
pub fn send_toggle() -> Result<()> {
    let app = gio::Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    app.register(None::<&gio::Cancellable>)?;
    if !app.is_remote() {
        anyhow::bail!("abla is not running. Start it from the application menu, or run `abla`.");
    }
    app.activate_action("toggle", None);
    flush(&app);
    Ok(())
}

/// Makes sure a remote action call has left the process before it exits.
fn flush(app: &impl IsA<gio::Application>) {
    if let Some(connection) = app.dbus_connection() {
        let _ = connection.flush_sync(None::<&gio::Cancellable>);
    }
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

impl App {
    fn ensure_button(self: &Rc<Self>) {
        if self.button.borrow().is_some() {
            return;
        }
        if self.config.borrow().is_ready() {
            self.create_button();
        } else {
            self.open_settings();
        }
    }

    fn create_button(self: &Rc<Self>) {
        let button = FloatingButton::new(self.clone());
        *self.button.borrow_mut() = Some(button);
    }

    fn open_settings(self: &Rc<Self>) {
        let app = self.clone();
        setup::show(&self.gtk, self.config.borrow().clone(), move |config| {
            *app.config.borrow_mut() = config;
            app.sync_action_states();
            let existing = app.button.borrow().clone();
            match existing {
                Some(button) => button.apply_config(),
                None => app.create_button(),
            }
        });
    }

    fn with_button(&self, f: impl FnOnce(&Rc<FloatingButton>)) {
        let button = self.button.borrow().clone();
        if let Some(button) = button {
            f(&button);
        }
    }

    fn save_config(&self) {
        if let Err(err) = self.config.borrow().save() {
            log::warn!("could not save the configuration: {err:#}");
        }
    }

    fn sync_action_states(&self) {
        let config = self.config.borrow();
        if let Some(action) = self.gtk.lookup_action("size").and_downcast::<gio::SimpleAction>() {
            action.set_state(&config.button.size.to_string().to_variant());
        }
        if let Some(action) = self.gtk.lookup_action("color").and_downcast::<gio::SimpleAction>() {
            action.set_state(&config.button.color.to_variant());
        }
        if let Some(action) = self.gtk.lookup_action("autostart").and_downcast::<gio::SimpleAction>() {
            action.set_state(&desktop::autostart_enabled().to_variant());
        }
    }
}

fn register_actions(app: &Rc<App>) {
    let toggle = gio::SimpleAction::new("toggle", None);
    {
        let app = app.clone();
        toggle.connect_activate(move |_, _| app.with_button(|b| b.toggle()));
    }
    app.gtk.add_action(&toggle);

    let show = gio::SimpleAction::new("show", None);
    {
        let app = app.clone();
        show.connect_activate(move |_, _| app.ensure_button());
    }
    app.gtk.add_action(&show);

    let settings = gio::SimpleAction::new("settings", None);
    {
        let app = app.clone();
        settings.connect_activate(move |_, _| app.open_settings());
    }
    app.gtk.add_action(&settings);

    let quit = gio::SimpleAction::new("quit", None);
    {
        let gtk_app = app.gtk.clone();
        quit.connect_activate(move |_, _| gtk_app.quit());
    }
    app.gtk.add_action(&quit);

    let size = gio::SimpleAction::new_stateful(
        "size",
        Some(glib::VariantTy::STRING),
        &app.config.borrow().button.size.to_string().to_variant(),
    );
    {
        let app = app.clone();
        size.connect_activate(move |action, parameter| {
            let Some(value) = parameter.and_then(|p| p.str()).and_then(|s| s.parse::<u32>().ok()) else {
                return;
            };
            action.set_state(&value.to_string().to_variant());
            app.config.borrow_mut().button.size = value;
            app.save_config();
            app.with_button(|b| b.apply_config());
        });
    }
    app.gtk.add_action(&size);

    let color = gio::SimpleAction::new_stateful(
        "color",
        Some(glib::VariantTy::STRING),
        &app.config.borrow().button.color.to_variant(),
    );
    {
        let app = app.clone();
        color.connect_activate(move |action, parameter| {
            let Some(value) = parameter.and_then(|p| p.str()) else {
                return;
            };
            action.set_state(&value.to_variant());
            app.config.borrow_mut().button.color = value.to_string();
            app.save_config();
            app.with_button(|b| b.apply_config());
        });
    }
    app.gtk.add_action(&color);

    let autostart = gio::SimpleAction::new_stateful("autostart", None, &desktop::autostart_enabled().to_variant());
    {
        let app = app.clone();
        autostart.connect_activate(move |action, _| {
            let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            match desktop::set_autostart(on) {
                Ok(()) => {
                    action.set_state(&on.to_variant());
                    app.config.borrow_mut().general.autostart = on;
                    app.save_config();
                }
                Err(err) => button::notify(&format!("Could not change autostart: {err:#}")),
            }
        });
    }
    app.gtk.add_action(&autostart);
}
