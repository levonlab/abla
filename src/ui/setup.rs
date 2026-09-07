//! First-run and settings window: provider, key, model, language, autostart.
//! The model list comes from the provider with the user's own key, so it is
//! always today's list and never one we typed in.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::time::Duration;

use gtk::prelude::*;
use gtk::glib;

use crate::config::Config;
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

pub fn show(app: &gtk::Application, config: Config, on_saved: impl Fn(Config) + 'static) {
    let window = gtk::Window::builder()
        .application(app)
        .title("Abla")
        .default_width(500)
        .resizable(false)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(28);
    root.set_margin_end(28);
    window.set_child(Some(&root));

    let title = gtk::Label::new(Some(if config.is_ready() { "Abla settings" } else { "Set up Abla" }));
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
    let current_kind = config.kind();
    provider.set_selected(ALL.iter().position(|k| *k == current_kind).unwrap_or(0) as u32);
    grid.attach(&field_label("Provider"), 0, 0, 1, 1);
    grid.attach(&provider, 1, 0, 1, 1);

    // Base URL (custom only)
    let base_label = field_label("Base URL");
    let base_url = gtk::Entry::new();
    base_url.set_placeholder_text(Some("https://your-host/v1"));
    base_url.set_text(&config.provider.base_url);
    grid.attach(&base_label, 0, 1, 1, 1);
    grid.attach(&base_url, 1, 1, 1, 1);

    // API key
    let key = gtk::PasswordEntry::new();
    key.set_show_peek_icon(true);
    key.set_text(&config.provider.api_key);
    grid.attach(&field_label("API key"), 0, 2, 1, 1);
    grid.attach(&key, 1, 2, 1, 1);

    let links = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let key_link = gtk::LinkButton::with_label("https://example.invalid", "Get a key");
    let price_link = gtk::LinkButton::with_label("https://example.invalid", "Prices");
    for l in [&key_link, &price_link] {
        l.add_css_class("flat");
    }
    links.append(&key_link);
    links.append(&price_link);
    grid.attach(&links, 1, 3, 1, 1);

    // Connect
    let connect_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.set_hexpand(true);
    connect_row.append(&connect);
    connect_row.append(&spinner);
    connect_row.append(&status);
    grid.attach(&connect_row, 1, 4, 1, 1);

    // Model
    let model_list = gtk::StringList::new(&[]);
    let model = gtk::DropDown::new(Some(model_list.clone()), None::<gtk::Expression>);
    let models: Rc<RefCell<Vec<Model>>> = Rc::new(RefCell::new(Vec::new()));
    if !config.provider.model.is_empty() {
        models.borrow_mut().push(Model { id: config.provider.model.clone(), realtime: config.provider.realtime });
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
        LANGUAGES.iter().position(|(_, code)| *code == config.provider.language).unwrap_or(0) as u32,
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

    let selected_kind = {
        let provider = provider.clone();
        move || ALL[provider.selected() as usize]
    };

    let refresh_save = {
        let save = save.clone();
        let key = key.clone();
        let models = models.clone();
        move || save.set_sensitive(!key.text().trim().is_empty() && !models.borrow().is_empty())
    };

    let apply_provider = {
        let base_url = base_url.clone();
        let base_label = base_label.clone();
        let key_link = key_link.clone();
        let price_link = price_link.clone();
        let selected_kind = selected_kind.clone();
        move |initial: bool| {
            let kind = selected_kind();
            let custom = kind == Kind::Custom;
            base_url.set_visible(custom);
            base_label.set_visible(custom);
            if !initial && !custom {
                base_url.set_text(kind.default_base_url());
            }
            if initial && custom && base_url.text().is_empty() {
                base_url.set_text("");
            }
            match kind.key_url() {
                Some(url) => {
                    key_link.set_uri(url);
                    key_link.set_visible(true);
                }
                None => key_link.set_visible(false),
            }
            match kind.pricing_url() {
                Some(url) => {
                    price_link.set_uri(url);
                    price_link.set_visible(true);
                }
                None => price_link.set_visible(false),
            }
        }
    };
    apply_provider(true);
    refresh_save();

    {
        let apply_provider = apply_provider.clone();
        let models = models.clone();
        let model_list = model_list.clone();
        let model = model.clone();
        let status = status.clone();
        let refresh_save = refresh_save.clone();
        let original_kind = current_kind;
        provider.connect_selected_notify(move |_| {
            apply_provider(false);
            // A different provider means a different model list.
            let kind = selected_kind();
            if kind != original_kind {
                models.borrow_mut().clear();
                model_list.splice(0, model_list.n_items(), &[]);
                model.set_sensitive(false);
                status.set_text("");
            }
            refresh_save();
        });
    }
    {
        let refresh_save = refresh_save.clone();
        key.connect_changed(move |_| refresh_save());
    }

    {
        let key = key.clone();
        let base_url = base_url.clone();
        let provider = provider.clone();
        let spinner = spinner.clone();
        let status = status.clone();
        let models = models.clone();
        let model_list = model_list.clone();
        let model = model.clone();
        let refresh_save = refresh_save.clone();
        let saved_model = config.provider.model.clone();
        connect.connect_clicked(move |button| {
            let kind = ALL[provider.selected() as usize];
            let api_key = key.text().trim().to_string();
            if api_key.is_empty() {
                status.set_text("Paste an API key first.");
                return;
            }
            let base = match normalize_base_url(&base_url.text()) {
                Ok(b) => b,
                Err(e) => {
                    status.set_text(&format!("{e}"));
                    return;
                }
            };
            button.set_sensitive(false);
            spinner.start();
            spinner.set_visible(true);
            status.set_text("Asking the provider which models this key can use…");
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(providers::list_models(kind, &base, &api_key));
            });
            let button = button.clone();
            let spinner = spinner.clone();
            let status = status.clone();
            let models = models.clone();
            let model_list = model_list.clone();
            let model = model.clone();
            let refresh_save = refresh_save.clone();
            let saved_model = saved_model.clone();
            glib::timeout_add_local(Duration::from_millis(80), move || {
                let outcome = match rx.try_recv() {
                    Ok(outcome) => outcome,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("the request was interrupted")),
                };
                spinner.stop();
                spinner.set_visible(false);
                button.set_sensitive(true);
                match outcome {
                    Ok(list) => {
                        let labels: Vec<String> = list.iter().map(model_label).collect();
                        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        model_list.splice(0, model_list.n_items(), &label_refs);
                        let selected = list.iter().position(|m| m.id == saved_model).unwrap_or(0);
                        *models.borrow_mut() = list;
                        model.set_selected(selected as u32);
                        model.set_sensitive(true);
                        let n = models.borrow().len();
                        status.set_text(&format!("Key accepted. {n} transcription model{}.", if n == 1 { "" } else { "s" }));
                    }
                    Err(err) => {
                        status.set_text(&format!("{err:#}"));
                    }
                }
                refresh_save();
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
            new_config.provider.name = kind.id().to_string();
            new_config.provider.base_url = base;
            new_config.provider.api_key = key.text().trim().to_string();
            new_config.provider.model = chosen.id;
            new_config.provider.realtime = chosen.realtime;
            new_config.provider.language = LANGUAGES[language.selected() as usize].1.to_string();
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
}

fn model_label(model: &Model) -> String {
    if model.realtime {
        format!("{} · live", model.id)
    } else {
        model.id.clone()
    }
}
