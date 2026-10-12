#![windows_subsystem = "windows"]
//! WBU Trainer: live cheats (memory), model swaps / big heads (via the dinput8.dll helper) and a
//! career save editor for Wakeboarding Unleashed (PC).
mod config;
mod game;
mod net;
mod save;
mod update;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};

use config::{Model, TrainerIni};
use save::RIDERS;
use game::{Command, Hotkey, Shared, hotkey_label};

const DEFAULT_GAME_DIR: &str = r"C:\Program Files (x86)\Aspyr\Wakeboarding Unleashed\Game";
const OK: Color32 = Color32::from_rgb(110, 200, 120);
const BAD: Color32 = Color32::from_rgb(230, 110, 90);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([620.0, 720.0]).with_title("WBU Trainer"),
        ..Default::default()
    };
    eframe::run_native("WBU Trainer", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Settings {
    game_dir: String,
    last_update_check: u64, // unix seconds of the last automatic update check
}

/// Keeps the installed helper in step with the one embedded in this trainer.
struct HelperSync {
    state: update::HelperState,
    next_check: std::time::Instant,
    wait_for_game: bool, // the user chose to wait until the game closes
    prompt: bool,        // "game is open" dialog visible
    confirm_close: bool, // second step of "Close the game"
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Live,
    Models,
    Save,
}

struct App {
    settings: Settings,
    shared: Arc<Mutex<Shared>>,
    tab: Tab,
    ini: TrainerIni,
    models: Vec<Model>,
    score_input: f32,
    saves: Vec<PathBuf>,
    save: Option<save::Save>,
    message: (String, bool), // (text, is error)
    updater: update::Shared,
    helper: HelperSync,
}

/// A full-width framed group.
fn section(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui);
    });
}

/// Display name of a Windows virtual-key code (letters, digits, a few common keys).
fn key_name(vk: i32) -> String {
    match vk {
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x10 | 0xA0 | 0xA1 => "Shift".into(),
        0x11 | 0xA2 | 0xA3 => "Ctrl".into(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        _ => format!("key {vk:#x}"),
    }
}

fn display_rider(name: &str) -> String {
    if name == "Shaun_sensai" { "Shaun (Sensei)".into() } else { name.into() }
}

/// Default game folder: next to the trainer, its parent, or the standard Aspyr install.
fn detect_game_dir() -> String {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let candidates = exe_dir.iter().flat_map(|d| [d.clone(), d.join("Game")]).chain([PathBuf::from(DEFAULT_GAME_DIR)]);
    for c in candidates {
        if c.join("WBU.EXE").is_file() {
            return c.to_string_lossy().into_owned();
        }
    }
    DEFAULT_GAME_DIR.into()
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let mut settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        if !Path::new(&settings.game_dir).join("WBU.EXE").is_file() {
            settings.game_dir = detect_game_dir();
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        game::spawn_worker(shared.clone(), cc.egui_ctx.clone());
        let mut app = Self {
            ini: TrainerIni::load(Path::new(&settings.game_dir)),
            settings,
            shared,
            tab: Tab::Live,
            models: Vec::new(),
            score_input: 100_000.0,
            saves: Vec::new(),
            save: None,
            message: (String::new(), false),
            updater: update::Shared::default(),
            helper: HelperSync {
                state: update::HelperState::Missing,
                next_check: std::time::Instant::now(),
                wait_for_game: false,
                prompt: false,
                confirm_close: false,
            },
        };
        update::cleanup_old();
        let args: Vec<String> = std::env::args().collect();
        if let Some(v) = args.iter().position(|a| a == "--updated-to").and_then(|i| args.get(i + 1)) {
            app.say(format!("Updated to version {v}."), false);
        }
        if update::due(app.settings.last_update_check) {
            app.settings.last_update_check = update::now();
            update::check(&app.updater, &cc.egui_ctx, false);
        }
        app.refresh_saves();
        app.models = config::models(&app.game_dir());
        app.save = app.saves.first().and_then(|p| save::Save::load(p).ok());
        app
    }

    fn game_dir(&self) -> PathBuf {
        PathBuf::from(&self.settings.game_dir)
    }

    fn game_ok(&self) -> bool {
        self.game_dir().join("WBU.EXE").is_file()
    }

    fn refresh_saves(&mut self) {
        self.saves = save::list(&self.game_dir());
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = (text.into(), error);
    }

    // ------------------------------------------------------------------ live

    fn live_tab(&mut self, ui: &mut egui::Ui) {
        let mut s = self.shared.lock().unwrap();
        match s.attached {
            Some(pid) => ui.colored_label(OK, format!("Attached to WBU.EXE (pid {pid})")),
            None => ui.colored_label(BAD, format!("{}", if s.problem.is_empty() { "Looking for the game…" } else { &s.problem })),
        };
        ui.add_space(6.0);

        section(ui, |ui| {
            ui.heading("Score");
            ui.horizontal(|ui| {
                ui.label("Current:");
                ui.label(RichText::new(format!("{:.0}", s.score)).strong().monospace());
                ui.label(format!("  combo {} × {:.1}", s.combo.max(0), s.combo_mult));
            });
            ui.small("The on-screen score catches up after your next trick.");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.score_input).speed(1000.0).range(0.0..=99_999_999.0));
                if ui.button("Set score").clicked() {
                    s.commands.push(Command::SetScore(self.score_input));
                }
                if ui.button("+10,000").on_hover_text(hotkey_label(Hotkey::AddScore)).clicked() {
                    s.commands.push(Command::AddScore(10_000.0));
                }
                if ui.button("+100,000").clicked() {
                    s.commands.push(Command::AddScore(100_000.0));
                }
            });
        });

        section(ui, |ui| {
            ui.heading("Cheats");
            let c = &mut s.cheats;
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.multiplier_on, "Score multiplier");
                ui.add(egui::Slider::new(&mut c.multiplier, 1.0..=20.0).suffix("×").step_by(0.5));
                ui.weak(hotkey_label(Hotkey::Multiplier));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.endless_combo, "Endless combo")
                    .on_hover_text("The combo timer never runs out, so a combo only ends when you land badly or fall.");
                ui.weak(hotkey_label(Hotkey::EndlessCombo));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.combo_mult_on, "Combo multiplier");
                ui.add(egui::Slider::new(&mut c.combo_mult, 1.0..=10.0).suffix("×").step_by(0.5));
                ui.weak(hotkey_label(Hotkey::ComboMultiplier));
            });
            ui.small("Hotkeys work while the game has focus (numeric keypad).");
        });

        section(ui, |ui| {
            ui.heading("Levels");
            let c = &mut s.cheats;
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.hidden_levels, "Florida & The Delta in free ride")
                    .on_hover_text("Free ride normally skips these two levels. Career mode already lists them.");
                ui.weak(hotkey_label(Hotkey::HiddenLevels));
            });
            ui.small("The cut Secret level: see the Models tab.");
        });

        let game_jump_label = s.game_jump_key.map_or("not found, using Space".to_string(), key_name);
        section(ui, |ui| {
            ui.heading("Riding");
            let c = &mut s.cheats;
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.unbreakable_rope, "Unbreakable rope")
                    .on_hover_text("The rope no longer snaps when it passes through objects. You can still drop it.");
                ui.weak(hotkey_label(Hotkey::UnbreakableRope));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.grab_anywhere, "Infinite rope grab range")
                    .on_hover_text("Press grab and the rope catches however far from the boat you are.");
                ui.weak(hotkey_label(Hotkey::GrabRange));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.no_crash, "No crashing")
                    .on_hover_text("You can't fall, bail or wipe out, so there's no respawn. Walls stop bouncing you back.");
                ui.weak(hotkey_label(Hotkey::NoCrash));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.double_jump, "Double jump anywhere")
                    .on_hover_text("Press jump on the water to jump from anywhere, and again in the air to jump again.");
                let game_key = game_jump_label;
                let label = |vk: i32, l: &str| if vk == 0 { format!("{l} ({game_key})") } else { l.to_string() };
                let key = game::JUMP_KEYS.iter().find(|k| k.0 == c.jump_key).map_or("?".into(), |k| label(k.0, k.1));
                egui::ComboBox::from_id_salt("jump_key").selected_text(key).width(150.0).show_ui(ui, |ui| {
                    for (vk, l) in game::JUMP_KEYS {
                        ui.selectable_value(&mut c.jump_key, vk, label(vk, l));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.checkbox(&mut c.unlimited_jumps, "Unlimited air jumps");
                ui.add(egui::Slider::new(&mut c.jump_strength, 5.0..=60.0).text("power"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.grind_speed, "No speed loss while grinding")
                    .on_hover_text("Keeps the speed you had when the grind started (or more).");
                ui.weak(hotkey_label(Hotkey::GrindSpeed));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.rope_off_speed, "No speed loss with the rope off")
                    .on_hover_text("Keeps the speed you had when you let go of the rope until you grab it again.");
                ui.weak(hotkey_label(Hotkey::RopeOffSpeed));
            });
        });

        section(ui, |ui| {
            ui.heading("Activity");
            for line in s.log.iter().rev() {
                ui.label(line);
            }
        });
        ui.add_space(4.0);
        ui.weak("Coming later: moon gravity / giant air, boat speed, infinite boost, instant special.");
    }

    // ------------------------------------------------------------------ models

    fn game_folder_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Game folder:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.settings.game_dir).desired_width(330.0));
            if ui.button("Browse…").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_directory(&self.settings.game_dir).pick_folder()
            {
                self.settings.game_dir = dir.to_string_lossy().into_owned();
                self.reload_folder();
            } else if r.lost_focus() {
                self.reload_folder();
            }
        });
        if !self.game_ok() {
            ui.colored_label(BAD, "WBU.EXE not found in this folder (pick the \"Game\" folder of the install).");
        }
    }

    fn reload_folder(&mut self) {
        self.ini = TrainerIni::load(&self.game_dir());
        self.models = config::models(&self.game_dir());
        self.refresh_saves();
        self.save = self.saves.first().and_then(|p| save::Save::load(p).ok());
    }

    fn helper_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| match self.helper.state {
            update::HelperState::Current | update::HelperState::Unknown => {
                ui.colored_label(OK, "Helper (dinput8.dll) installed");
            }
            update::HelperState::Outdated => {
                ui.colored_label(Color32::from_rgb(230, 190, 90), "Helper (dinput8.dll) out of date: it is updated when the game is closed");
            }
            update::HelperState::Missing => {
                ui.colored_label(BAD, "Helper (dinput8.dll) not installed: model changes need it");
                if !update::HELPER.is_empty() && ui.button("Install").clicked() {
                    match update::install_helper(&self.game_dir()) {
                        Ok(()) => self.say("Helper installed. Start (or restart) the game to use it.", false),
                        Err(e) => self.say(format!("Could not install the helper: {e}"), true),
                    }
                    self.helper.next_check = std::time::Instant::now();
                }
            }
        });
    }

    /// Every couple of seconds: is the installed helper current? Update it when the game is closed;
    /// if the game is open, ask (close it now, or wait).
    fn sync_helper(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        if now >= self.helper.next_check {
            self.helper.next_check = now + std::time::Duration::from_secs(2);
            self.helper.state = update::helper_state(&self.game_dir());
            if self.helper.state == update::HelperState::Outdated {
                if game::find_pid().is_none() {
                    match update::install_helper(&self.game_dir()) {
                        Ok(()) => {
                            self.say("Helper (dinput8.dll) updated to match this trainer.", false);
                            self.helper.state = update::HelperState::Current;
                        }
                        Err(e) => self.say(format!("Could not update the helper: {e}"), true),
                    }
                    self.helper.prompt = false;
                    self.helper.wait_for_game = false;
                } else if !self.helper.wait_for_game {
                    self.helper.prompt = true;
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        if !self.helper.prompt {
            return;
        }
        egui::Window::new("Helper update")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("The helper (dinput8.dll) needs updating, but the game is open.");
                ui.label("It can only be replaced while the game is closed.");
                ui.add_space(6.0);
                if self.helper.confirm_close {
                    ui.colored_label(BAD, "Close the game now? Anything not saved in the game will be lost.");
                    ui.horizontal(|ui| {
                        if ui.button("Close it now").clicked() {
                            match game::terminate_game() {
                                Ok(()) => self.say("Game closed; updating the helper…", false),
                                Err(e) => self.say(format!("Could not close the game: {e}"), true),
                            }
                            self.helper.prompt = false;
                            self.helper.confirm_close = false;
                            self.helper.wait_for_game = true;
                            self.helper.next_check = std::time::Instant::now() + std::time::Duration::from_secs(1);
                        }
                        if ui.button("Back").clicked() {
                            self.helper.confirm_close = false;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Close the game").clicked() {
                            self.helper.confirm_close = true;
                        }
                        if ui.button("Wait until I close it").clicked() {
                            self.helper.prompt = false;
                            self.helper.wait_for_game = true;
                            self.say("The helper will be updated as soon as the game closes.", false);
                        }
                    });
                }
            });
    }

    /// Update banner + release notes window.
    fn update_ui(&mut self, ui: &mut egui::Ui) {
        let status = self.updater.lock().unwrap().status.clone();
        let dismissed = self.updater.lock().unwrap().dismissed.clone();
        let ctx = ui.ctx().clone();
        let banner = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Panel::top("update").show(ui, |ui| {
                ui.horizontal(|ui| add(ui));
            });
        };
        match status {
            update::Status::Available(r) if dismissed.as_deref() != Some(r.version.as_str()) => {
                banner(ui, &mut |ui| {
                    ui.colored_label(OK, format!("Version {} is available (you have {}).", r.version, update::VERSION));
                    if ui.button("What's new / Update").clicked() {
                        self.updater.lock().unwrap().show_notes = true;
                    }
                    if ui.button("Later").clicked() {
                        self.updater.lock().unwrap().dismissed = Some(r.version.clone());
                    }
                });
                let mut open = self.updater.lock().unwrap().show_notes;
                if open {
                    egui::Window::new(format!("What's new in {}", r.version))
                        .open(&mut open)
                        .collapsible(false)
                        .default_width(440.0)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                ui.label(if r.notes.is_empty() { "No release notes." } else { r.notes.as_str() });
                            });
                            ui.separator();
                            ui.horizontal(|ui| {
                                if ui.button(RichText::new("Update now").strong()).clicked() {
                                    update::install(&self.updater, &ctx, r.clone());
                                }
                                if !r.page.is_empty() && ui.link("Release page").clicked() {
                                    ctx.open_url(egui::OpenUrl::new_tab(&r.page));
                                }
                            });
                            ui.small("The download is checked against its published SHA-256 checksum before anything is replaced.");
                        });
                    self.updater.lock().unwrap().show_notes = open;
                }
            }
            update::Status::Downloading(r, done, total) => banner(ui, &mut |ui| {
                ui.label(format!("Downloading {}…", r.version));
                let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).desired_width(220.0).show_percentage());
            }),
            update::Status::Installing => banner(ui, &mut |ui| {
                ui.spinner();
                ui.label("Installing the update…");
            }),
            update::Status::Restarting => {
                banner(ui, &mut |ui| {
                    ui.label("Restarting into the new version…");
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            update::Status::Error(e) => banner(ui, &mut |ui| {
                ui.colored_label(BAD, e.clone());
                if ui.small_button("OK").clicked() {
                    self.updater.lock().unwrap().status = update::Status::Idle;
                }
            }),
            update::Status::UpToDate if self.updater.lock().unwrap().show_notes => banner(ui, &mut |ui| {
                ui.label(format!("You have the latest version ({}).", update::VERSION));
                if ui.small_button("OK").clicked() {
                    self.updater.lock().unwrap().show_notes = false;
                }
            }),
            _ => {}
        }
    }

    /// A "looks like" picker: (normal) or any model, grouped by folder. Returns true on change.
    fn look_picker(ui: &mut egui::Ui, id: &str, models: &[Model], value: &mut String) -> bool {
        let current = models.iter().find(|m| m.folder == *value).map_or("(normal)".to_string(), |m| format!("{} / {}", m.group, m.name));
        let mut changed = false;
        egui::ComboBox::from_id_salt(id).selected_text(current).width(260.0).height(420.0).show_ui(ui, |ui| {
            changed |= ui.selectable_value(value, String::new(), "(normal)").changed();
            let mut group = "";
            for m in models {
                if m.group != group {
                    group = &m.group;
                    ui.separator();
                    ui.weak(group);
                }
                changed |= ui.selectable_value(value, m.folder.clone(), format!("   {}", m.name)).changed();
            }
        });
        changed
    }

    /// Slider that reports a change only when the user lets go (each value is applied in game).
    fn live_slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
        let r = ui.add(egui::Slider::new(value, range).step_by(0.05).suffix("×"));
        r.drag_stopped() || (r.changed() && !r.dragged())
    }

    fn turn_buttons(ui: &mut egui::Ui, turn: &mut u32) -> bool {
        ui.label("Turn");
        let mut changed = false;
        for (q, label) in [(0, "0°"), (1, "90°"), (2, "180°"), (3, "270°")] {
            changed |= ui.selectable_value(turn, q, label).changed();
        }
        changed
    }

    fn models_tab(&mut self, ui: &mut egui::Ui) {
        self.game_folder_row(ui);
        self.helper_status(ui);
        ui.add_space(4.0);
        ui.label("Changes show up in the game right away (the helper must be installed before the game starts).");

        let mut changed = false;
        let models = std::mem::take(&mut self.models);
        section(ui, |ui| {
            ui.heading("Rider");
            ui.horizontal(|ui| {
                ui.label("Looks like");
                changed |= Self::look_picker(ui, "rider", &models, &mut self.ini.rider);
            });
            let rigid = !self.ini.rider.is_empty() && !self.ini.rider.starts_with("players\\");
            if rigid {
                ui.horizontal(|ui| {
                    ui.label("Size");
                    changed |= Self::live_slider(ui, &mut self.ini.rider_size, 0.2..=4.0);
                    changed |= Self::turn_buttons(ui, &mut self.ini.rider_turn);
                });
                ui.small("Non-rider models ride stiffly on the wakeboard (they have no rider animations).");
            } else if !self.ini.rider.is_empty() {
                ui.horizontal(|ui| changed |= Self::turn_buttons(ui, &mut self.ini.rider_turn));
            }
            ui.horizontal(|ui| {
                ui.label("Big head");
                changed |= Self::live_slider(ui, &mut self.ini.big_head, 0.3..=3.0);
            });
        });
        section(ui, |ui| {
            ui.heading("Boat");
            ui.horizontal(|ui| {
                ui.label("Looks like");
                changed |= Self::look_picker(ui, "boat", &models, &mut self.ini.boat);
            });
            ui.horizontal(|ui| {
                ui.label("Size");
                changed |= Self::live_slider(ui, &mut self.ini.boat_scale, 0.3..=3.0);
                if !self.ini.boat.is_empty() {
                    changed |= Self::turn_buttons(ui, &mut self.ini.boat_turn);
                }
            });
        });
        self.models = models;
        section(ui, |ui| {
            ui.heading("Secret level");
            ui.label("The game has a finished but unreachable level, \"Secret\". Play it in place of another level:");
            ui.horizontal(|ui| {
                ui.label("Replace");
                let label = if self.ini.secret_in.is_empty() { "(off)".to_string() } else { self.ini.secret_in.clone() };
                egui::ComboBox::from_id_salt("secret").selected_text(label).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut self.ini.secret_in, String::new(), "(off)").changed();
                    for host in config::SECRET_HOSTS {
                        changed |= ui.selectable_value(&mut self.ini.secret_in, host.to_string(), host).changed();
                    }
                });
                ui.weak("with Secret");
            });
            ui.small("Applies the next time you enter that level, even while the game is running. The replaced level's challenges and \
                      people still use their own positions, so free ride works best.");
        });
        if !self.ini.redirects.is_empty() {
            ui.weak(format!("{} folder redirect line(s) in wbu-trainer.ini are kept.", self.ini.redirects.len()));
        }
        ui.horizontal(|ui| {
            if ui.button("Reset all").clicked() {
                self.ini = TrainerIni { redirects: std::mem::take(&mut self.ini.redirects), ..Default::default() };
                changed = true;
            }
            let running = self.shared.lock().unwrap().attached.is_some() || game::find_pid().is_some();
            if ui.add_enabled(self.game_ok() && !running, egui::Button::new("▶ Launch game")).clicked() {
                let dir = self.game_dir();
                match std::process::Command::new(dir.join("WBU.EXE")).current_dir(&dir).spawn() {
                    Ok(_) => self.say("Game launched.", false),
                    Err(e) => self.say(format!("Could not start the game: {e}"), true),
                }
            }
        });
        if changed && self.game_ok() {
            self.write_ini();
        }
    }

    fn write_ini(&mut self) {
        match self.ini.save(&self.game_dir()) {
            Ok(()) => self.say("Applied.", false),
            Err(e) => self.say(format!("Could not write wbu-trainer.ini: {e}"), true),
        }
    }

    // ------------------------------------------------------------------ save editor

    fn save_tab(&mut self, ui: &mut egui::Ui) {
        self.game_folder_row(ui);
        let running = self.shared.lock().unwrap().attached.is_some();
        if running {
            ui.colored_label(BAD, "Close the game before editing saves (it would overwrite them).");
        }
        ui.horizontal(|ui| {
            ui.label("Career save:");
            let current = self.save.as_ref().map(|s| s.path.clone());
            let label = current.as_ref().and_then(|p| p.file_name()).map_or("(pick one)".into(), |n| n.to_string_lossy().into_owned());
            let mut pick = None;
            egui::ComboBox::from_id_salt("save").selected_text(label).show_ui(ui, |ui| {
                for p in &self.saves {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    if ui.selectable_label(current.as_ref() == Some(p), name).clicked() {
                        pick = Some(p.clone());
                    }
                }
            });
            if ui.button("⟳").on_hover_text("Rescan the Save folder").clicked() {
                self.refresh_saves();
            }
            if let Some(p) = pick {
                match save::Save::load(&p) {
                    Ok(s) => self.save = Some(s),
                    Err(e) => self.say(format!("{}: {e}", p.display()), true),
                }
            }
        });
        if self.saves.is_empty() {
            ui.label("No career saves yet. Start a Career in the game and save once.");
        }
        let Some(sv) = &mut self.save else { return };
        ui.label(format!("Profile: {}", sv.name()));

        section(ui, |ui| {
            ui.heading("Unlocks");
            ui.horizontal(|ui| {
                let tip = "All boards, secret boarders, videos, perfect balance, free-ride levels, \
                           100% career and max stats for every rider";
                if ui.button(RichText::new("Unlock everything").strong()).on_hover_text(tip).clicked() {
                    sv.unlock_everything();
                }
                ui.weak("(what the game's own Ultimate cheat does, for every rider)");
            });
            egui::Grid::new("unlocks").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, name: &str, status: String, done: bool| {
                    ui.label(name);
                    let color = if done { OK } else { ui.visuals().weak_text_color() };
                    ui.colored_label(color, status);
                    let clicked = ui.add_enabled(!done, egui::Button::new("Unlock")).clicked();
                    ui.end_row();
                    clicked
                };
                if row(ui, "Boards", format!("{} / 30", sv.boards_unlocked()), sv.boards_unlocked() == 30) {
                    sv.unlock_boards();
                }
                if row(ui, "Secret boarders", format!("{} / 3", sv.boarders_unlocked()), sv.boarders_unlocked() == 3) {
                    sv.unlock_boarders();
                }
                if row(ui, "Videos", format!("{} / 7", sv.videos_unlocked()), sv.videos_unlocked() == 7) {
                    sv.unlock_videos();
                }
                let pb = sv.perfect_balance();
                if row(ui, "Perfect balance", if pb { "unlocked" } else { "locked" }.into(), pb) {
                    sv.unlock_perfect_balance();
                }
                let locked = sv.free_ride_locked();
                let status = if locked == 0 { "all open".to_string() } else { format!("{locked} locked") };
                if row(ui, "Free-ride levels", status, locked == 0) {
                    sv.unlock_free_ride();
                }
            });
        });

        section(ui, |ui| {
            ui.heading("Riders");
            ui.spacing_mut().interact_size.x = 28.0; // compact stat boxes
            egui::Grid::new("stats").striped(true).spacing([4.0, 4.0]).show(ui, |ui| {
                ui.label("");
                for k in 0..save::STAT_COUNT {
                    ui.weak(format!("S{}", k + 1)).on_hover_text(format!("Stat {} (max {})", k + 1, save::MAX_STAT));
                }
                ui.label("");
                ui.weak("Career");
                ui.label("");
                ui.end_row();
                for (r, name) in RIDERS.iter().enumerate() {
                    ui.label(display_rider(name));
                    for k in 0..save::STAT_COUNT {
                        let mut v = sv.stat(r, k);
                        if ui.add(egui::DragValue::new(&mut v).range(0..=save::MAX_STAT)).changed() {
                            sv.set_stat(r, k, v);
                        }
                    }
                    if ui.small_button("Max").on_hover_text("Max stats").clicked() {
                        sv.max_stats(r);
                    }
                    let pct = sv.progress(r) * 100.0;
                    ui.label(format!("{:>3.0}%  {}/11", pct, sv.levels_beaten(r)))
                        .on_hover_text("Career progress (goals, challenges, gaps) / levels beaten");
                    let tip = "Every goal, challenge and gap done and every level beaten (unlocks this rider's career levels)";
                    if ui.add_enabled(pct < 100.0, egui::Button::new("100%").small()).on_hover_text(tip).clicked() {
                        sv.complete_career(r);
                    }
                    ui.end_row();
                }
            });
        });

        let dirty = sv.dirty();
        let mut result = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty && !running, egui::Button::new("Write save (keeps a backup)")).clicked() {
                result = Some(sv.write());
            }
            if ui.add_enabled(dirty, egui::Button::new("Undo changes")).clicked() {
                sv.revert();
            }
        });
        match result {
            Some(Ok(backup)) => self.say(format!("Saved. Backup: {}", backup.display()), false),
            Some(Err(e)) => self.say(format!("Could not write the save: {e}"), true),
            None => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Live, "Live cheats");
                ui.selectable_value(&mut self.tab, Tab::Models, "Models");
                ui.selectable_value(&mut self.tab, Tab::Save, "Save editor");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let checking = matches!(self.updater.lock().unwrap().status, update::Status::Checking);
                    if ui.add_enabled(!checking, egui::Button::new(if checking { "Checking…" } else { "Check for updates" }).small()).clicked() {
                        self.settings.last_update_check = update::now();
                        self.updater.lock().unwrap().show_notes = true; // also shows "up to date"
                        update::check(&self.updater, ui.ctx(), true);
                    }
                    ui.weak(format!("v{}", update::VERSION));
                });
            });
        });
        self.update_ui(ui);
        self.sync_helper(&ui.ctx().clone());
        egui::Panel::bottom("status").show(ui, |ui| {
            let (text, err) = &self.message;
            ui.colored_label(if *err { BAD } else { ui.visuals().weak_text_color() }, if text.is_empty() { " " } else { text });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Live => self.live_tab(ui),
                Tab::Models => self.models_tab(ui),
                Tab::Save => self.save_tab(ui),
            });
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}
