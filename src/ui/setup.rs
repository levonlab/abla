//! First-run and settings window: provider, key, model, language, autostart.
//! The model list comes from the provider with the user's own key, so it is
//! always today's list and never one we typed in.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::config::{Config, Provider};
use crate::desktop;
use crate::providers::{self, ALL, Kind, Model, normalize_base_url};

const LANGUAGES: [(&str, &str); 18] = [
    ("Detect automatically", ""),
    ("English", "en"),
    ("Español", "es"),
    ("Français", "fr"),
    ("Deutsch", "de"),
    ("Italiano", "it"),
    ("Português", "pt"),
    ("Català", "ca"),
    ("Nederlands", "nl"),
    ("Polski", "pl"),
    ("Русский", "ru"),
    ("Українська", "uk"),
    ("Türkçe", "tr"),
    ("العربية", "ar"),
    ("हिन्दी", "hi"),
    ("中文", "zh"),
    ("日本語", "ja"),
    ("한국어", "ko"),
];

/// Provider, base URL and key, as a unit: what a check is about.
type Target = (Kind, String, String);

pub fn show(app: &gtk::Application, config: Config, on_saved: impl Fn(Config) + 'static) {
    let window = gtk::Window::builder()
        .application(app)
        .title("Abla")
        .default_width(520)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(28);
    root.set_margin_end(28);
    window.set_child(Some(&root));

    let title = gtk::Label::new(Some(if config.is_ready() {
        "Abla settings"
    } else {
        "Set up Abla"
    }));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    root.append(&title);

    let intro = gtk::Label::new(Some(
        "Pick a provider, paste your API key and choose a model. \
         The key stays in a private file in your home folder.",
    ));
    intro.add_css_class("dim-label");
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    root.append(&intro);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(10);
    grid.set_column_spacing(14);
    root.append(&grid);

    let field_label = |text: &str| {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(1.0);
        l.add_css_class("dim-label");
        l
    };

    // Provider
    let labels: Vec<&str> = ALL.iter().map(|k| k.label()).collect();
    let provider = gtk::DropDown::from_strings(&labels);
    provider.set_hexpand(true);
    let saved_kind = config.kind();
    provider.set_selected(ALL.iter().position(|k| *k == saved_kind).unwrap_or(0) as u32);
    grid.attach(&field_label("Provider"), 0, 0, 1, 1);
    grid.attach(&provider, 1, 0, 1, 1);

    // Base URL (custom only)
    let base_label = field_label("Base URL");
    let base_url = gtk::Entry::new();
    base_url.set_placeholder_text(Some("https://your-host/v1"));
    base_url.set_text(&config.provider.base_url);
    grid.attach(&base_label, 0, 1, 1, 1);
    grid.attach(&base_url, 1, 1, 1, 1);

    // API key. An ABLA_API_KEY in the environment counts as a key without
    // ever being written to the file.
    let env_key = std::env::var("ABLA_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    let key = gtk::PasswordEntry::new();
    key.set_show_peek_icon(true);
    key.set_text(&config.provider.api_key);
    if env_key.is_some() && config.provider.api_key.is_empty() {
        key.set_placeholder_text(Some("Using ABLA_API_KEY from the environment"));
    }
    grid.attach(&field_label("API key"), 0, 2, 1, 1);
    grid.attach(&key, 1, 2, 1, 1);

    let key_link = gtk::LinkButton::with_label("https://example.invalid", "Get a key");
    key_link.add_css_class("flat");
    key_link.set_halign(gtk::Align::Start);
    grid.attach(&key_link, 1, 3, 1, 1);

    // Connect, status and the two ways out of a bad key
    let connect_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    let change_key = gtk::Button::with_label("Use another key");
    change_key.add_css_class("flat");
    change_key.set_visible(false);
    let restore_key = gtk::Button::with_label("Back to the working key");
    restore_key.add_css_class("flat");
    restore_key.set_visible(false);
    connect_row.append(&connect);
    connect_row.append(&spinner);
    connect_row.append(&status);
    connect_row.append(&change_key);
    connect_row.append(&restore_key);
    grid.attach(&connect_row, 1, 4, 1, 1);

    // Model
    let model_list = gtk::StringList::new(&[]);
    let model = gtk::DropDown::new(Some(model_list.clone()), None::<gtk::Expression>);
    let models: Rc<RefCell<Vec<Model>>> = Rc::new(RefCell::new(Vec::new()));
    let models_kind = Rc::new(Cell::new(saved_kind));
    if !config.provider.model.is_empty() {
        models.borrow_mut().push(Model {
            id: config.provider.model.clone(),
            realtime: config.provider.realtime,
        });
        model_list.append(&model_label(&models.borrow()[0]));
    }
    model.set_sensitive(!models.borrow().is_empty());
    grid.attach(&field_label("Model"), 0, 5, 1, 1);
    grid.attach(&model, 1, 5, 1, 1);

    let model_hint = gtk::Label::new(Some("Models marked “live” show the text while you speak."));
    model_hint.add_css_class("dim-label");
    model_hint.set_xalign(0.0);
    model_hint.set_wrap(true);
    grid.attach(&model_hint, 1, 6, 1, 1);

    // Language
    let language_labels: Vec<&str> = LANGUAGES.iter().map(|(l, _)| *l).collect();
    let language = gtk::DropDown::from_strings(&language_labels);
    language.set_selected(
        LANGUAGES
            .iter()
            .position(|(_, code)| *code == config.provider.language)
            .unwrap_or(0) as u32,
    );
    grid.attach(&field_label("Language"), 0, 7, 1, 1);
    grid.attach(&language, 1, 7, 1, 1);

    // Autostart
    let autostart = gtk::Switch::new();
    autostart.set_active(desktop::autostart_enabled() || config.general.autostart);
    autostart.set_halign(gtk::Align::Start);
    autostart.set_valign(gtk::Align::Center);
    grid.attach(&field_label("Start at login"), 0, 8, 1, 1);
    grid.attach(&autostart, 1, 8, 1, 1);

    // Save
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    actions.append(&save);
    root.append(&actions);

    // ---- behaviour -------------------------------------------------------

    let effective_key = {
        let key = key.clone();
        let env_key = env_key.clone();
        move || {
            let typed = key.text().trim().to_string();
            if typed.is_empty() {
                env_key.clone().unwrap_or_default()
            } else {
                typed
            }
        }
    };

    let current_target = {
        let provider = provider.clone();
        let base_url = base_url.clone();
        let effective_key = effective_key.clone();
        move || -> Target {
            let kind = ALL[provider.selected() as usize];
            let base = normalize_base_url(&base_url.text()).unwrap_or_default();
            (kind, base, effective_key())
        }
    };

    // What was on screen when the window opened: saving that again needs
    // no new check, so language and autostart can change offline.
    let saved_target: Target = (
        saved_kind,
        normalize_base_url(&config.provider.base_url).unwrap_or_default(),
        if config.provider.api_key.is_empty() {
            env_key.clone().unwrap_or_default()
        } else {
            config.provider.api_key.clone()
        },
    );
    // The last provider and key the provider accepted, and the last it refused.
    let last_good: Rc<RefCell<Option<Target>>> = Rc::new(RefCell::new(None));
    let last_bad: Rc<RefCell<Option<(Target, String)>>> = Rc::new(RefCell::new(None));

    let refresh_save = {
        let save = save.clone();
        let current_target = current_target.clone();
        let last_good = last_good.clone();
        let models = models.clone();
        let saved_target = saved_target.clone();
        move || {
            let target = current_target();
            let checked = last_good.borrow().as_ref() == Some(&target) || target == saved_target;
            save.set_sensitive(checked && !target.2.is_empty() && !models.borrow().is_empty());
        }
    };

    // The status line always describes the provider and key on screen:
    // green once they are accepted, red when they were refused, blank when
    // they have not been tried. Connect only shows for the untried ones.
    let refresh_connect = {
        let connect = connect.clone();
        let change_key = change_key.clone();
        let restore_key = restore_key.clone();
        let status = status.clone();
        let last_good = last_good.clone();
        let last_bad = last_bad.clone();
        let current_target = current_target.clone();
        move || {
            let target = current_target();
            status.remove_css_class("success");
            status.remove_css_class("error");
            let good = last_good.borrow().as_ref() == Some(&target);
            let bad = last_bad
                .borrow()
                .as_ref()
                .filter(|(t, _)| *t == target)
                .map(|(_, e)| e.clone());
            if good {
                status.set_text("✓ Connected");
                status.add_css_class("success");
                connect.set_visible(false);
                change_key.set_visible(true);
                restore_key.set_visible(false);
            } else if let Some(error) = bad {
                status.set_text(&format!("✗ {error}"));
                status.add_css_class("error");
                connect.set_visible(true);
                change_key.set_visible(false);
                restore_key
                    .set_visible(last_good.borrow().as_ref().is_some_and(|g| g.0 == target.0));
            } else {
                status.set_text("");
                connect.set_visible(true);
                change_key.set_visible(false);
                restore_key.set_visible(false);
            }
        }
    };

    let apply_provider = {
        let base_url = base_url.clone();
        let base_label = base_label.clone();
        let key_link = key_link.clone();
        let provider = provider.clone();
        move |initial: bool| {
            let kind = ALL[provider.selected() as usize];
            let custom = kind == Kind::Custom;
            base_url.set_visible(custom);
            base_label.set_visible(custom);
            if !initial && !custom {
                base_url.set_text(kind.default_base_url());
            }
            match kind.key_url() {
                Some(url) => {
                    key_link.set_uri(url);
                    key_link.set_visible(true);
                }
                None => key_link.set_visible(false),
            }
        }
    };
    apply_provider(true);
    refresh_save();
    refresh_connect();

    {
        let apply_provider = apply_provider.clone();
        let models = models.clone();
        let models_kind = models_kind.clone();
        let model_list = model_list.clone();
        let model = model.clone();
        let key = key.clone();
        let base_url = base_url.clone();
        let connect = connect.clone();
        let effective_key = effective_key.clone();
        let refresh_save = refresh_save.clone();
        let refresh_connect = refresh_connect.clone();
        let config = config.clone();
        provider.connect_selected_notify(move |provider| {
            apply_provider(false);
            let kind = ALL[provider.selected() as usize];
            if kind == models_kind.get() {
                return;
            }
            // Each provider has its own key and model list. Bring back what
            // was last used with this one, or start clean.
            models.borrow_mut().clear();
            model_list.splice(0, model_list.n_items(), &[]);
            model.set_sensitive(false);
            models_kind.set(kind);
            let remembered = config.remembered_for(kind).unwrap_or_default();
            key.set_text(&remembered.api_key);
            if kind == Kind::Custom && !remembered.base_url.is_empty() {
                base_url.set_text(&remembered.base_url);
            }
            if !remembered.model.is_empty() {
                let known = Model {
                    id: remembered.model.clone(),
                    realtime: remembered.realtime,
                };
                model_list.append(&model_label(&known));
                models.borrow_mut().push(known);
                model.set_selected(0);
                model.set_sensitive(true);
            }
            refresh_save();
            refresh_connect();
            if !effective_key().is_empty() {
                connect.emit_clicked();
            }
        });
    }
    {
        let refresh_save = refresh_save.clone();
        let refresh_connect = refresh_connect.clone();
        key.connect_changed(move |_| {
            refresh_save();
            refresh_connect();
        });
    }
    {
        let refresh_save = refresh_save.clone();
        let refresh_connect = refresh_connect.clone();
        base_url.connect_changed(move |_| {
            refresh_save();
            refresh_connect();
        });
    }
    {
        let key = key.clone();
        change_key.connect_clicked(move |_| {
            key.set_text("");
            key.grab_focus();
        });
    }
    {
        let key = key.clone();
        let base_url = base_url.clone();
        let last_good = last_good.clone();
        let env_key = env_key.clone();
        restore_key.connect_clicked(move |_| {
            let Some((kind, base, good_key)) = last_good.borrow().clone() else {
                return;
            };
            if kind == Kind::Custom {
                base_url.set_text(&base);
            }
            // A key that came from the environment is restored by clearing the field.
            key.set_text(if env_key.as_deref() == Some(good_key.as_str()) {
                ""
            } else {
                &good_key
            });
        });
    }

    {
        let current_target = current_target.clone();
        let spinner = spinner.clone();
        let status = status.clone();
        let models = models.clone();
        let models_kind = models_kind.clone();
        let model_list = model_list.clone();
        let model = model.clone();
        let refresh_save = refresh_save.clone();
        let refresh_connect = refresh_connect.clone();
        let last_good = last_good.clone();
        let last_bad = last_bad.clone();
        let saved_model = config.provider.model.clone();
        connect.connect_clicked(move |button| {
            let target = current_target();
            let (kind, base, api_key) = target.clone();
            if api_key.is_empty() {
                status.set_text("Paste an API key first.");
                return;
            }
            if base.is_empty() {
                status.set_text("The base URL must start with https://");
                return;
            }
            button.set_sensitive(false);
            spinner.start();
            spinner.set_visible(true);
            status.remove_css_class("error");
            status.remove_css_class("success");
            status.set_text("Checking the key…");
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(providers::list_models(kind, &base, &api_key));
            });
            let button = button.clone();
            let spinner = spinner.clone();
            let models = models.clone();
            let models_kind = models_kind.clone();
            let model_list = model_list.clone();
            let model = model.clone();
            let refresh_save = refresh_save.clone();
            let refresh_connect = refresh_connect.clone();
            let last_good = last_good.clone();
            let last_bad = last_bad.clone();
            let saved_model = saved_model.clone();
            glib::timeout_add_local(Duration::from_millis(80), move || {
                let result = match rx.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return glib::ControlFlow::Continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Err(anyhow::anyhow!("the request was interrupted"))
                    }
                };
                spinner.stop();
                spinner.set_visible(false);
                button.set_sensitive(true);
                match result {
                    Ok(list) => {
                        let labels: Vec<String> = list.iter().map(model_label).collect();
                        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        model_list.splice(0, model_list.n_items(), &label_refs);
                        let selected = list.iter().position(|m| m.id == saved_model).unwrap_or(0);
                        *models.borrow_mut() = list;
                        models_kind.set(kind);
                        model.set_selected(selected as u32);
                        model.set_sensitive(true);
                        *last_good.borrow_mut() = Some(target.clone());
                        if last_bad
                            .borrow()
                            .as_ref()
                            .is_some_and(|(t, _)| *t == target)
                        {
                            *last_bad.borrow_mut() = None;
                        }
                    }
                    Err(err) => {
                        *last_bad.borrow_mut() =
                            Some((target.clone(), capitalize(&format!("{err:#}"))));
                    }
                }
                refresh_save();
                refresh_connect();
                glib::ControlFlow::Break
            });
        });
    }

    {
        let window = window.clone();
        let base_url = base_url.clone();
        let status = status.clone();
        let models = models.clone();
        let on_saved = Rc::new(on_saved);
        save.connect_clicked(move |_| {
            let kind = ALL[provider.selected() as usize];
            let Some(chosen) = models.borrow().get(model.selected() as usize).cloned() else {
                status.set_text("Connect and choose a model first.");
                return;
            };
            let base = match normalize_base_url(&base_url.text()) {
                Ok(b) => b,
                Err(e) => {
                    status.set_text(&format!("{e}"));
                    return;
                }
            };
            let mut new_config = config.clone();
            new_config.switch_provider(Provider {
                name: kind.id().to_string(),
                base_url: base,
                api_key: key.text().trim().to_string(),
                model: chosen.id,
                realtime: chosen.realtime,
                language: LANGUAGES[language.selected() as usize].1.to_string(),
            });
            new_config.general.autostart = autostart.is_active();
            if let Err(err) = desktop::set_autostart(autostart.is_active()) {
                log::warn!("autostart: {err:#}");
            }
            if let Err(err) = new_config.save() {
                status.set_text(&format!("Could not save: {err:#}"));
                return;
            }
            on_saved(new_config);
            window.close();
        });
    }

    spinner.set_visible(false);
    window.present();
    // With a key at hand, fetch today's model list straight away.
    if !effective_key().is_empty() {
        connect.emit_clicked();
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn model_label(model: &Model) -> String {
    if model.realtime {
        format!("{} · live", model.id)
    } else {
        model.id.clone()
    }
}
