//! Career saves (`Game\Save\CAREER n.CAR`): an 8-byte header (data size, 0) followed by the raw
//! career block; no checksum. Each rider has a 0x250-byte record; its 7 stats are 4-byte ints.
use std::path::{Path, PathBuf};

/// Rider folders in save-file order (matches the exe's rider table).
pub const RIDERS: [&str; 10] = ["Shaun", "Cobe", "Colin", "Dallas", "Shapiro", "Parks", "Tara", "Summer", "Jordan", "Shaun_sensai"];
pub const SIZE: usize = 8200;
// File offsets. The save is the career struct from +0x47e8 (file = struct offset - 0x47e0).
const STATS: usize = 0x334; // rider 0's 7 stats (max 10, as the game's own Ultimate cheat sets them)
const RIDER_STRIDE: usize = 0x250;
const PROGRESS: usize = 0x134; // rider 0: 16 levels x 8 bit-array dwords (goals, challenges, gaps)
const LEVELS_BEATEN: usize = 0x350; // rider 0: bit per beaten level (1..11)
const BOARDS: usize = 0x1834; // bit 3*rider + board (3 unlockable boards per rider)
const VIDEOS: usize = 0x1844; // bit per rider (riders 0..6)
const BOARDERS: usize = 0x1848; // bits 0..2: secret boarders
const EXTRAS: usize = 0x182c; // bit 4: perfect balance
const BALANCE_ON: usize = 0x186c;
const FREE_RIDE_LOCKS: usize = 0x1858; // bit per level, set = locked
pub const STAT_COUNT: usize = 7;
pub const MAX_STAT: i32 = 10;

pub struct Save {
    pub path: PathBuf,
    data: Vec<u8>,
    original: Vec<u8>,
}

pub fn list(game_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(game_dir.join("Save"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("car")))
        .collect();
    out.sort();
    out
}

impl Save {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        if data.len() != SIZE || u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize != SIZE - 8 {
            return Err("Not a career save this editor understands".into());
        }
        Ok(Self { path: path.to_path_buf(), original: data.clone(), data })
    }

    pub fn name(&self) -> String {
        let raw = &self.data[8..40];
        String::from_utf8_lossy(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())]).into_owned()
    }

    fn at(rider: usize, stat: usize) -> usize {
        STATS + rider * RIDER_STRIDE + stat * 4
    }

    pub fn stat(&self, rider: usize, stat: usize) -> i32 {
        let o = Self::at(rider, stat);
        i32::from_le_bytes(self.data[o..o + 4].try_into().unwrap())
    }

    pub fn set_stat(&mut self, rider: usize, stat: usize, v: i32) {
        let o = Self::at(rider, stat);
        self.data[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn u32_at(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.data[o..o + 4].try_into().unwrap())
    }

    fn set_u32(&mut self, o: usize, v: u32) {
        self.data[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub fn boards_unlocked(&self) -> u32 {
        (self.u32_at(BOARDS) & 0x3fff_ffff).count_ones()
    }

    pub fn unlock_boards(&mut self) {
        self.set_u32(BOARDS, self.u32_at(BOARDS) | 0x3fff_ffff);
    }

    pub fn boarders_unlocked(&self) -> u32 {
        (self.u32_at(BOARDERS) & 7).count_ones()
    }

    pub fn unlock_boarders(&mut self) {
        self.set_u32(BOARDERS, self.u32_at(BOARDERS) | 7);
    }

    pub fn videos_unlocked(&self) -> u32 {
        (self.u32_at(VIDEOS) & 0x7f).count_ones()
    }

    pub fn unlock_videos(&mut self) {
        self.set_u32(VIDEOS, self.u32_at(VIDEOS) | 0x7f);
    }

    pub fn perfect_balance(&self) -> bool {
        self.u32_at(EXTRAS) & 0x10 != 0
    }

    pub fn unlock_perfect_balance(&mut self) {
        self.set_u32(EXTRAS, self.u32_at(EXTRAS) | 0x10);
        self.set_u32(BALANCE_ON, 1);
    }

    pub fn free_ride_locked(&self) -> u32 {
        (self.u32_at(FREE_RIDE_LOCKS) & 0xffff).count_ones()
    }

    pub fn unlock_free_ride(&mut self) {
        self.set_u32(FREE_RIDE_LOCKS, 0);
    }

    /// Career levels beaten by a rider (of 11).
    pub fn levels_beaten(&self, rider: usize) -> u32 {
        (self.u32_at(LEVELS_BEATEN + rider * RIDER_STRIDE) & 0xffe).count_ones()
    }

    /// Share of career progress bits set for a rider (0..1): every goal, challenge and gap.
    pub fn progress(&self, rider: usize) -> f32 {
        let base = PROGRESS + rider * RIDER_STRIDE;
        let set: u32 = (0..16 * 8).map(|k| self.u32_at(base + k * 4).count_ones()).sum();
        set as f32 / (16 * 8 * 32) as f32
    }

    /// 100% career for a rider, the way the game's own Ultimate cheat does it: every goal,
    /// challenge and gap of every level done, every level beaten. Career levels unlock from this.
    pub fn complete_career(&mut self, rider: usize) {
        let base = PROGRESS + rider * RIDER_STRIDE;
        for k in 0..16 * 8 {
            self.set_u32(base + k * 4, 0xffff_ffff);
        }
        let o = LEVELS_BEATEN + rider * RIDER_STRIDE;
        self.set_u32(o, self.u32_at(o) | 0xffe);
    }

    pub fn max_stats(&mut self, rider: usize) {
        (0..STAT_COUNT).for_each(|k| self.set_stat(rider, k, MAX_STAT));
    }

    pub fn unlock_everything(&mut self) {
        self.unlock_boards();
        self.unlock_boarders();
        self.unlock_videos();
        self.unlock_perfect_balance();
        self.unlock_free_ride();
        for r in 0..RIDERS.len() {
            self.complete_career(r);
            self.max_stats(r);
        }
    }

    pub fn dirty(&self) -> bool {
        self.data != self.original
    }

    pub fn revert(&mut self) {
        self.data = self.original.clone();
    }

    /// Writes the save after copying the file on disk to `Save\wbu-trainer-backup\` (a folder the
    /// game does not scan). Returns the backup path.
    pub fn write(&mut self) -> Result<PathBuf, String> {
        let dir = self.path.parent().unwrap().join("wbu-trainer-backup");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let backup = dir.join(format!("{}.{stamp}.bak", self.path.file_name().unwrap().to_string_lossy()));
        std::fs::copy(&self.path, &backup).map_err(|e| format!("backup failed: {e}"))?;
        std::fs::write(&self.path, &self.data).map_err(|e| e.to_string())?;
        self.original = self.data.clone();
        Ok(backup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Unlock everything" may only touch the fields it documents.
    #[test]
    fn unlock_everything_touches_only_known_fields() {
        // Needs a real career save (none is included): set WBU_TEST_SAVE to its path.
        let Some(path) = std::env::var_os("WBU_TEST_SAVE").map(std::path::PathBuf::from).filter(|p| p.is_file()) else {
            eprintln!("skipped: set WBU_TEST_SAVE to a career save (.CAR)");
            return;
        };
        let mut s = Save::load(&path).expect("sample save");
        s.unlock_everything();
        let mut allowed = vec![false; SIZE];
        let mut allow = |o: usize, n: usize| allowed[o..o + n].iter_mut().for_each(|a| *a = true);
        for r in 0..RIDERS.len() {
            allow(PROGRESS + r * RIDER_STRIDE, 16 * 8 * 4);
            allow(STATS + r * RIDER_STRIDE, STAT_COUNT * 4);
            allow(LEVELS_BEATEN + r * RIDER_STRIDE, 4);
        }
        for o in [BOARDS, VIDEOS, BOARDERS, EXTRAS, BALANCE_ON, FREE_RIDE_LOCKS] {
            allow(o, 4);
        }
        for (i, (a, b)) in s.original.iter().zip(&s.data).enumerate() {
            assert!(a == b || allowed[i], "unexpected change at {i:#x}");
        }
        assert!(PROGRESS + 16 * 8 * 4 == STATS, "progress runs straight into the stats");
        assert_eq!(s.boards_unlocked(), 30);
        assert_eq!(s.free_ride_locked(), 0);
        assert_eq!(s.levels_beaten(0), 11);
    }
}
