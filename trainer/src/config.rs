//! `Game\wbu-trainer.ini`: settings the in-game helper (dinput8.dll) re-reads while the game runs.
use std::path::Path;

pub struct TrainerIni {
    pub big_head: f32,
    pub boat_scale: f32,
    pub rider: String, // model folder under data\ ("" = normal), e.g. spawns\bayou\gator_walk02
    pub rider_size: f32,
    pub rider_turn: u32, // quarter turns
    pub boat: String,
    pub boat_turn: u32,
    pub redirects: Vec<String>, // hand-written [redirect] lines, kept as is
    /// Level folder whose files are replaced by the cut Secret level ("" = off), e.g. "Florida".
    pub secret_in: String,
}

/// Levels the Secret level can stand in for (folder names under data\levels).
pub const SECRET_HOSTS: [&str; 12] =
    ["Florida", "TheDelta", "LakePowell", "Bayou", "Springfield", "HongKong", "Belize", "Venice", "Boneyard", "TimberLake", "OceanWorld", "Credits"];

impl Default for TrainerIni {
    fn default() -> Self {
        Self {
            big_head: 1.0,
            boat_scale: 1.0,
            rider: String::new(),
            rider_size: 1.0,
            rider_turn: 0,
            boat: String::new(),
            boat_turn: 0,
            redirects: Vec::new(),
            secret_in: String::new(),
        }
    }
}

impl TrainerIni {
    pub fn load(game_dir: &Path) -> Self {
        let mut ini = Self::default();
        let text = std::fs::read_to_string(game_dir.join("wbu-trainer.ini")).unwrap_or_default();
        let mut section = String::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line.trim_matches(['[', ']']).to_lowercase();
                continue;
            }
            if line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            if section == "redirect" {
                let level = |p: &str| {
                    let p = p.trim().replace('/', "\\").to_lowercase();
                    p.strip_prefix("data\\levels\\").map(|f| f.trim_end_matches('\\').to_string())
                };
                if let (Some(from), Some(to)) = (level(key), level(value))
                    && to == "secret"
                    && let Some(host) = SECRET_HOSTS.iter().find(|h| h.eq_ignore_ascii_case(&from))
                {
                    ini.secret_in = host.to_string();
                } else {
                    ini.redirects.push(line.to_string());
                }
                continue;
            }
            let (key, value) = (key.trim().to_lowercase(), value.trim());
            let num = |d: f32| value.parse::<f32>().ok().filter(|v| v.is_finite()).unwrap_or(d);
            match key.as_str() {
                "big_head" => ini.big_head = num(1.0),
                "boat_scale" => ini.boat_scale = num(1.0),
                "rider" => ini.rider = value.to_lowercase(),
                "rider_size" => ini.rider_size = num(1.0),
                "rider_turn" => ini.rider_turn = value.parse().unwrap_or(0) % 4,
                "boat" => ini.boat = value.to_lowercase(),
                "boat_turn" => ini.boat_turn = value.parse().unwrap_or(0) % 4,
                _ => {}
            }
        }
        ini
    }

    pub fn save(&self, game_dir: &Path) -> std::io::Result<()> {
        let mut out = String::from("; Written by the WBU trainer. The helper (dinput8.dll) applies changes while the game runs.\n");
        out += &format!("[fun]\nbig_head={:.2}\nboat_scale={:.2}\n\n", self.big_head, self.boat_scale);
        out += &format!("[look]\nrider={}\nrider_size={:.2}\nrider_turn={}\n", self.rider, self.rider_size, self.rider_turn);
        out += &format!("boat={}\nboat_turn={}\n", self.boat, self.boat_turn);
        if !self.redirects.is_empty() || !self.secret_in.is_empty() {
            out += "\n; Folder redirects, applied the next time the game opens those files\n[redirect]\n";
            if !self.secret_in.is_empty() {
                out += &format!("data\\levels\\{} = data\\levels\\Secret\n", self.secret_in);
            }
            for line in &self.redirects {
                out += line;
                out.push('\n');
            }
        }
        std::fs::write(game_dir.join("wbu-trainer.ini"), out)
    }
}

/// A model the rider or boat can look like.
pub struct Model {
    pub folder: String, // lower-case, relative to data\, e.g. "spawns\bayou\gator_walk02"
    pub group: String,  // "Riders", "Boats", or the level name
    pub name: String,
}

/// Every folder under data\ with a model file, grouped like the game's folders.
pub fn models(game_dir: &Path) -> Vec<Model> {
    let data = game_dir.join("data");
    let has_model = |p: &Path| ["player.bin", "boat.bin", "object.bin"].iter().any(|f| p.join(f).is_file());
    let subdirs = |p: &Path| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        v.sort();
        v
    };
    let name_of = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut out = Vec::new();
    for (dir, group) in [("players", "Riders"), ("boats", "Boats")] {
        for p in subdirs(&data.join(dir)) {
            if has_model(&p) {
                let name = name_of(&p);
                out.push(Model { folder: format!("{dir}\\{name}").to_lowercase(), group: group.into(), name: name.replace('_', " ") });
            }
        }
    }
    for level in subdirs(&data.join("Spawns")) {
        for p in subdirs(&level) {
            if has_model(&p) {
                let name = name_of(&p);
                out.push(Model {
                    folder: format!("spawns\\{}\\{name}", name_of(&level)).to_lowercase(),
                    group: name_of(&level),
                    name: name.replace('_', " "),
                });
            }
        }
    }
    out
}
