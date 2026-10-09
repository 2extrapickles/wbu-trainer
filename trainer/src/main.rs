#![windows_subsystem = "windows"]
//! WBU Trainer: live cheats (memory), model swaps / big heads (via the dinput8.dll helper) and a
//! career save editor for Wakeboarding Unleashed (PC).
mod config;
mod game;
mod save;

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
struct Settings {
    game_dir: String,
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
        };
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
        let installed = std::fs::read(self.game_dir().join("dinput8.dll"))
            .is_ok_and(|d| d.windows(13).any(|w| w == b"wbu-trainer.i"));
        ui.horizontal(|ui| {
            if installed {
                ui.colored_label(OK, "Helper (dinput8.dll) installed");
            } else {
                ui.colored_label(BAD, "Helper (dinput8.dll) not installed: model changes need it");
                let bundled = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("dinput8.dll")));
                if let Some(src) = bundled.filter(|p| p.is_file())
                    && ui.button("Install").clicked()
                {
                    match std::fs::copy(&src, self.game_dir().join("dinput8.dll")) {
                        Ok(_) => self.say("Helper installed.", false),
                        Err(e) => self.say(format!("Could not install the helper: {e}"), true),
                    }
                }
            }
        });
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
            ui.small("Applies when the game starts (restart it if it is running). The replaced level's challenges and \
                      people still use their own positions, so free ride works best.");
        });
        if !self.ini.redirects.is_empty() {
            ui.weak(format!("{} folder redirect line(s) in wbu-trainer.ini are kept (applied at game start).", self.ini.redirects.len()));
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
            });
        });
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
