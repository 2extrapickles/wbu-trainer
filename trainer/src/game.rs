//! Live cheats: find the running WBU.EXE, then a worker thread reads and writes its memory and
//! watches the global hotkeys. All addresses are static (.bss of the 2003 Aspyr build).
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAllocEx, VirtualProtectEx};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

const SCORE: u32 = 0x8b7ccc; // f32, the master score (the HUD copy refreshes on the next trick)
const COMBO_COUNT: u32 = 0x8b7cdc; // i32, tricks in the current combo
const COMBO_TIMER: u32 = 0x8b7ce8; // f32, refilled to 1.5 s after each trick
// Combo banking (FUN_005374f0, score struct 0x8a33c8): points = MULT (+0x14900) x BASE (+0x148fc),
// then SCORE (+0x14904) += points and both reset to 1.0 / 0. (0x8b7cf0, used before, is not in the formula.)
const COMBO_MULT: u32 = 0x8b7cc8; // f32, the combo multiplier the points are computed with
const COMBO_BASE: u32 = 0x8b7cc4; // f32, points of the current combo before the multiplier
/// Score multiplier: the bank adds `fild [esp+0xc]` (int points) to SCORE at 0x537596..0x5375ac. We jump
/// to a small stub that multiplies the points by a factor we keep in the stub, then jump back.
const BANK_SITE: u32 = 0x537596;
const BANK_ORIGINAL: [u8; 22] = [
    0xdb, 0x44, 0x24, 0x0c, // fild dword [esp+0xc]
    0x89, 0x86, 0x24, 0x49, 0x01, 0x00, // mov [esi+0x14924], eax   (last combo points, for the HUD)
    0xd8, 0x86, 0x04, 0x49, 0x01, 0x00, // fadd dword [esi+0x14904] (score)
    0xd9, 0x9e, 0x04, 0x49, 0x01, 0x00, // fstp dword [esi+0x14904]
];
const BANK_RESUME: u32 = 0x5375ac;
const STUB_FACTOR: u32 = 0x40; // offset of the f32 factor inside the stub
const SIGNATURE: (u32, &[u8]) = (0x5c1440, b"WBU CD2\0"); // only write to the build we researched

// Rider (player 1). `[PLAYER_BLOCK_PTR]` = per-player block (0x8a1318 for player 1); its physics object
// is at +0x20 (mode i32 at +0x20: 1 = locked to a rail), velocity vec3 at +0x15f0, state words at
// +0x18a60.. (names in the exe's condition table at 0x5b4018).
const PLAYER_BLOCK_PTR: u32 = 0x8d2958;
const PHYS_MODE: u32 = 0x20;
const VELOCITY: u32 = 0x15f0;
const STATE_W0: u32 = 0x18a60; // 0x8000 rail_ride, 0x40 water_contact, 0x2 airborne
const STATE_W2: u32 = 0x18a68; // 0x4 holding_rope, 0x8 not_holding_rope
const RAIL_RIDE: u32 = 0x8000;
const AIRBORNE: u32 = 0x2;
const SURFACE_CONTACT: u32 = 0x41; // ground_contact | water_contact

/// Keys offered for double jump: (virtual-key code, label). 0 = the game's own jump key.
pub const JUMP_KEYS: [(i32, &str); 7] = [
    (0, "Game's jump key"),
    (0x20, "Space"),
    (0xA0, "Left Shift"),
    (0xA2, "Left Ctrl"),
    (0x45, "E"),
    (0x51, "Q"),
    (0x46, "F"),
];
/// Slot of the jump action in config.cfg ("keyset=0", k0..k11 = DirectInput scancodes).
const JUMP_SLOT: &str = "k4";

/// The game's keyboard jump key as a Windows virtual-key code, from <game>\config.cfg.
fn game_jump_vk(exe_dir: &std::path::Path) -> Option<i32> {
    let text = std::fs::read_to_string(exe_dir.join("config.cfg")).ok()?;
    let mut in_keyset0 = false;
    for line in text.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("keyset=") {
            in_keyset0 = v.trim() == "0";
        } else if line.starts_with("padset=") {
            in_keyset0 = false;
        } else if in_keyset0 && let Some(v) = line.strip_prefix(&format!("{JUMP_SLOT}=")) {
            let dik: u32 = v.trim().parse().ok().filter(|&c: &u32| c > 0 && c < 0x100)?;
            // DirectInput codes are scancodes; codes >= 0x80 are the 0xE0-prefixed (extended) keys.
            let scan = if dik >= 0x80 { 0xE000 | (dik & 0x7F) } else { dik };
            let vk = unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) };
            return (vk != 0).then_some(vk as i32);
        }
    }
    None
}
const HOLDING_ROPE: u32 = 0x4;

/// The per-frame "rope line passes through level geometry" check calls rip-rope (0x51fa20) from here.
/// NOP-ing the call keeps the rope through obstacles; dropping it with the button is a separate path.
const ROPE_RIP_CALL: u32 = 0x522df6;
const ROPE_RIP_ORIGINAL: [u8; 5] = [0xE8, 0x25, 0xCC, 0xFF, 0xFF]; // call 0x51fa20
const NOP5: [u8; 5] = [0x0F, 0x1F, 0x44, 0x00, 0x00];

/// Grab range: pressing grab on a free rope opens a grab window (phys+0x1bd0 > 0); 0x51f7b0 then
/// catches only if the rider-to-boat distance [ROPE_DISTANCE] is within reach = handle length
/// (phys+0x1bd8, 2.5) + the level's rope length; while the window is open a held rope that is
/// stretched past reach drops again. The cheat stretches the handle length to cover the distance
/// while you call for the rope, then reels it back in once you hold it (no code patch).
/// v0.4.1 patched this jump instead; it is restored to the original on attach.
const GRAB_RANGE_JUMP: u32 = 0x51f914;
const GRAB_RANGE_ORIGINAL: [u8; 6] = [0x0F, 0x8A, 0xFE, 0x00, 0x00, 0x00]; // jp 0x51fa18
const NOP6: [u8; 6] = [0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00];
const ROPE_DISTANCE: u32 = 0x8e01a0; // f32, rider to boat
const ROPE_STATE: u32 = 0x16adc; // block: 0 holding, 1 dropped, 2 free, 3 catching
const GRAB_WINDOW: u32 = 0x20 + 0x1bd0; // block: f32 > 0 while calling for the rope
const HANDLE_LEN: u32 = 0x20 + 0x1bd8; // block: f32
const LEVEL_ROPE_TABLE: u32 = 0x20 + 0x19e8; // block: ptr; + 0x3ef0 + idx*4 = level rope length
const LEVEL_ROPE_INDEX: u32 = 0x20 + 0x1be0; // block: i32
const REEL_SPEED: f32 = 6.0; // units per second the stretched rope shortens while held

/// Crashes: every crash (walls, bad landings, rails...) starts in FUN_00511040, which plays the crash
/// animation and sets the crash bits; wall hits then run the bounce FUN_005110c0, which sets
/// "collision" (also a crash state). A `ret` at both entries (cdecl) means no crash and no respawn.
const START_CRASH: u32 = 0x511040; // push edi
const CRASH_BOUNCE: u32 = 0x5110c0; // sub esp, 0x20
const RET: [u8; 1] = [0xC3];

/// Free ride hides levels 4 (Florida) and 8 (The Delta): the level select skips them with
/// `cmp eax,4 / je / cmp eax,8` in three places (first pick 0x499c1d, browsing 0x499fd3 / 0x49a020).
/// Changing the 4 and 8 to -1 (no such level) lets them through. Career mode lists them already.
const HIDDEN_LEVEL_CHECKS: [u32; 3] = [0x499c1d, 0x499fd3, 0x49a020];

/// Hotkeys (numeric keypad) and what they do; also shown in the window.
pub const HOTKEYS: [(i32, &str, Hotkey); 10] = [
    (0x61, "Num 1", Hotkey::Multiplier),
    (0x62, "Num 2", Hotkey::EndlessCombo),
    (0x63, "Num 3", Hotkey::ComboMultiplier),
    (0x64, "Num 4", Hotkey::AddScore),
    (0x65, "Num 5", Hotkey::UnbreakableRope),
    (0x66, "Num 6", Hotkey::GrindSpeed),
    (0x67, "Num 7", Hotkey::RopeOffSpeed),
    (0x68, "Num 8", Hotkey::GrabRange),
    (0x69, "Num 9", Hotkey::NoCrash),
    (0x60, "Num 0", Hotkey::HiddenLevels),
];

#[derive(Clone, Copy, PartialEq)]
pub enum Hotkey {
    Multiplier,
    EndlessCombo,
    ComboMultiplier,
    AddScore,
    UnbreakableRope,
    GrindSpeed,
    RopeOffSpeed,
    GrabRange,
    NoCrash,
    HiddenLevels,
}

pub fn hotkey_label(which: Hotkey) -> &'static str {
    HOTKEYS.iter().find(|h| h.2 == which).map_or("", |h| h.1)
}

#[derive(Clone)]
pub struct Cheats {
    pub multiplier_on: bool,
    pub multiplier: f32,
    pub endless_combo: bool,
    pub combo_mult_on: bool,
    pub combo_mult: f32,
    pub unbreakable_rope: bool,
    pub grind_speed: bool,
    pub rope_off_speed: bool,
    pub grab_anywhere: bool,
    pub no_crash: bool,
    pub hidden_levels: bool,
    pub double_jump: bool,
    pub jump_key: i32, // 0 = the game's jump key
    pub jump_strength: f32,
    pub unlimited_jumps: bool,
}

impl Default for Cheats {
    fn default() -> Self {
        Self {
            multiplier_on: false,
            multiplier: 3.0,
            endless_combo: false,
            combo_mult_on: false,
            combo_mult: 5.0,
            unbreakable_rope: false,
            grind_speed: false,
            rope_off_speed: false,
            grab_anywhere: false,
            no_crash: false,
            hidden_levels: false,
            double_jump: false,
            jump_key: 0,
            jump_strength: 15.0,
            unlimited_jumps: true,
        }
    }
}

pub enum Command {
    AddScore(f32),
    SetScore(f32),
}

#[derive(Default)]
pub struct Shared {
    pub game_jump_key: Option<i32>, // read from the game's config.cfg (shown in the UI)
    pub cheats: Cheats,
    pub commands: Vec<Command>,
    pub attached: Option<u32>, // pid
    pub problem: String,       // why we are not attached
    pub score: f32,
    pub combo: i32,
    pub combo_mult: f32,
    pub log: Vec<String>,
}

impl Shared {
    fn say(&mut self, msg: impl Into<String>) {
        self.log.push(msg.into());
        if self.log.len() > 8 {
            self.log.remove(0);
        }
    }
}

struct Process {
    handle: usize, // HANDLE as usize so the worker thread can own it
    pid: u32,
    dir: std::path::PathBuf, // folder of WBU.EXE
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle as _) };
    }
}

pub fn find_pid() -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("WBU.EXE") {
                found = Some(entry.th32ProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

impl Process {
    fn attach() -> Result<Self, String> {
        let pid = find_pid().ok_or("Game not running")?;
        let access = PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_LIMITED_INFORMATION;
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err("Cannot open WBU.EXE (try running the trainer as administrator)".into());
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0;
        let exe = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..if ok { len as usize } else { 0 }]));
        let dir = exe.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let p = Process { handle: handle as usize, pid, dir };
        let mut sig = vec![0u8; SIGNATURE.1.len()];
        if !p.read(SIGNATURE.0, &mut sig) || sig != SIGNATURE.1 {
            return Err("Unknown WBU.EXE version: live cheats are disabled".into());
        }
        Ok(p)
    }

    fn read(&self, addr: u32, buf: &mut [u8]) -> bool {
        let mut n = 0usize;
        let ok = unsafe {
            ReadProcessMemory(self.handle as _, addr as usize as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut n)
        };
        ok != 0 && n == buf.len()
    }

    fn write(&self, addr: u32, buf: &[u8]) -> bool {
        let mut n = 0usize;
        let ok = unsafe {
            WriteProcessMemory(self.handle as _, addr as usize as *const c_void, buf.as_ptr().cast(), buf.len(), &mut n)
        };
        ok != 0 && n == buf.len()
    }

    fn f32(&self, addr: u32) -> Option<f32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b).then(|| f32::from_le_bytes(b))
    }

    fn i32(&self, addr: u32) -> Option<i32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b).then(|| i32::from_le_bytes(b))
    }

    fn set_f32(&self, addr: u32, v: f32) -> bool {
        self.write(addr, &v.to_le_bytes())
    }

    fn u32(&self, addr: u32) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b).then(|| u32::from_le_bytes(b))
    }

    fn vec3(&self, addr: u32) -> Option<[f32; 3]> {
        let mut b = [0u8; 12];
        self.read(addr, &mut b).then(|| [0, 1, 2].map(|k| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap())))
    }

    fn set_vec3(&self, addr: u32, v: [f32; 3]) -> bool {
        let b: Vec<u8> = v.iter().flat_map(|c| c.to_le_bytes()).collect();
        self.write(addr, &b)
    }

    /// Write to the game's code: only over the exact bytes we expect (original or our patch).
    fn patch_code(&self, addr: u32, expect: &[&[u8]], new: &[u8]) -> bool {
        let mut cur = vec![0u8; new.len()];
        if !self.read(addr, &mut cur) || !expect.contains(&cur.as_slice()) {
            return false;
        }
        if cur == new {
            return true;
        }
        let mut old = 0u32;
        unsafe {
            let a = addr as usize as *const c_void;
            if VirtualProtectEx(self.handle as _, a, new.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
                return false;
            }
            let ok = self.write(addr, new);
            VirtualProtectEx(self.handle as _, a, new.len(), old, &mut old);
            ok
        }
    }

    /// Install (or find the already installed) score-multiplier stub; returns the factor address.
    fn bank_stub(&self) -> Option<u32> {
        let mut cur = [0u8; 22];
        if !self.read(BANK_SITE, &mut cur) {
            return None;
        }
        if cur[0] == 0xE9 {
            // Installed by an earlier trainer run: reuse it (the stub lives as long as the game).
            let stub = (BANK_SITE as i64 + 5 + i32::from_le_bytes(cur[1..5].try_into().unwrap()) as i64) as u32;
            let mut head = [0u8; 4];
            return (self.read(stub, &mut head) && head == BANK_ORIGINAL[..4]).then_some(stub + STUB_FACTOR);
        }
        if cur != BANK_ORIGINAL {
            return None;
        }
        let stub = unsafe {
            VirtualAllocEx(self.handle as _, std::ptr::null(), 0x100, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
        } as usize as u32;
        if stub == 0 {
            return None;
        }
        let factor = stub + STUB_FACTOR;
        let mut code: Vec<u8> = vec![0xdb, 0x44, 0x24, 0x0c]; // fild dword [esp+0xc]
        code.extend([0xd8, 0x0d]); // fmul dword [factor]
        code.extend(factor.to_le_bytes());
        code.extend([0xdb, 0x96, 0x24, 0x49, 0x01, 0x00]); // fist dword [esi+0x14924]
        code.extend(&BANK_ORIGINAL[10..22]); // fadd / fstp score
        let jmp_at = stub + code.len() as u32;
        code.push(0xE9); // jmp back
        code.extend(((BANK_RESUME as i64 - (jmp_at as i64 + 5)) as i32).to_le_bytes());
        code.resize(STUB_FACTOR as usize, 0xCC);
        code.extend(1.0f32.to_le_bytes());
        if !self.write(stub, &code) {
            return None;
        }
        let mut patch = vec![0xE9];
        patch.extend(((stub as i64 - (BANK_SITE as i64 + 5)) as i32).to_le_bytes());
        patch.resize(22, 0x90);
        self.patch_code(BANK_SITE, &[&BANK_ORIGINAL], &patch).then_some(factor)
    }

    fn alive(&self) -> bool {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.handle as _, &mut code) != 0 && code == 259 } // STILL_ACTIVE
    }
}

fn sane_score(v: f32) -> bool {
    v.is_finite() && (0.0..1e9).contains(&v)
}

/// Runs for the lifetime of the app: (re)attaches to the game, applies the cheats ~100 times a
/// second and handles hotkeys even while the game window has focus.
pub fn spawn_worker(shared: Arc<Mutex<Shared>>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let mut game: Option<Process> = None;
        let mut next_attach = Instant::now();
        let mut keys_down = [false; HOTKEYS.len()];
        let mut rope_patched: Option<bool> = None; // what is in game memory (None = unknown, re-apply)
        let mut reach_patched: Option<bool> = None;
        let mut crash_patched: Option<bool> = None;
        let mut levels_patched: Option<bool> = None;
        let mut bank_factor: Option<Option<u32>> = None; // None = not looked yet
        let mut speed = SpeedLock::default();
        let mut reach = RopeReach::default();
        let mut jump = DoubleJump::default();
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let mut changed = false;

            // Hotkeys work whether or not the game is attached (toggles are remembered).
            for (i, (vk, _, which)) in HOTKEYS.iter().enumerate() {
                let down = unsafe { GetAsyncKeyState(*vk) } as u16 & 0x8000 != 0;
                if down && !keys_down[i] {
                    let mut s = shared.lock().unwrap();
                    let c = &mut s.cheats;
                    let msg = match which {
                        Hotkey::Multiplier => {
                            c.multiplier_on = !c.multiplier_on;
                            format!("Score multiplier {}", if c.multiplier_on { "on" } else { "off" })
                        }
                        Hotkey::EndlessCombo => {
                            c.endless_combo = !c.endless_combo;
                            format!("Endless combo {}", if c.endless_combo { "on" } else { "off" })
                        }
                        Hotkey::ComboMultiplier => {
                            c.combo_mult_on = !c.combo_mult_on;
                            format!("Combo multiplier lock {}", if c.combo_mult_on { "on" } else { "off" })
                        }
                        Hotkey::UnbreakableRope => {
                            c.unbreakable_rope = !c.unbreakable_rope;
                            format!("Unbreakable rope {}", if c.unbreakable_rope { "on" } else { "off" })
                        }
                        Hotkey::GrindSpeed => {
                            c.grind_speed = !c.grind_speed;
                            format!("No speed loss grinding {}", if c.grind_speed { "on" } else { "off" })
                        }
                        Hotkey::RopeOffSpeed => {
                            c.rope_off_speed = !c.rope_off_speed;
                            format!("No speed loss rope off {}", if c.rope_off_speed { "on" } else { "off" })
                        }
                        Hotkey::GrabRange => {
                            c.grab_anywhere = !c.grab_anywhere;
                            format!("Infinite rope grab range {}", if c.grab_anywhere { "on" } else { "off" })
                        }
                        Hotkey::NoCrash => {
                            c.no_crash = !c.no_crash;
                            format!("No crashing {}", if c.no_crash { "on" } else { "off" })
                        }
                        Hotkey::HiddenLevels => {
                            c.hidden_levels = !c.hidden_levels;
                            format!("Florida & The Delta in free ride {}", if c.hidden_levels { "on" } else { "off" })
                        }
                        Hotkey::AddScore => {
                            s.commands.push(Command::AddScore(10_000.0));
                            String::new()
                        }
                    };
                    if !msg.is_empty() {
                        s.say(msg);
                    }
                    changed = true;
                }
                keys_down[i] = down;
            }

            if game.as_ref().is_some_and(|g| !g.alive()) {
                game = None;
                rope_patched = None;
                reach_patched = None;
                crash_patched = None;
                levels_patched = None;
                bank_factor = None;
                speed = SpeedLock::default();
                reach = RopeReach::default();
                jump = DoubleJump::default();
                let mut s = shared.lock().unwrap();
                s.attached = None;
                s.say("Game closed");
                changed = true;
            }
            if game.is_none() && Instant::now() >= next_attach {
                next_attach = Instant::now() + Duration::from_secs(1);
                let mut s = shared.lock().unwrap();
                match Process::attach() {
                    Ok(p) => {
                        s.attached = Some(p.pid);
                        s.problem.clear();
                        s.say(format!("Attached to WBU.EXE (pid {})", p.pid));
                        game = Some(p);
                    }
                    Err(e) => s.problem = e,
                }
                changed = true;
            }

            if let Some(g) = &game {
                let (Some(mut score), Some(combo), Some(mult)) = (g.f32(SCORE), g.i32(COMBO_COUNT), g.f32(COMBO_MULT))
                else {
                    continue;
                };
                let mut s = shared.lock().unwrap();
                let cheats = s.cheats.clone();
                for cmd in std::mem::take(&mut s.commands) {
                    let target = match cmd {
                        Command::AddScore(v) => score + v,
                        Command::SetScore(v) => v,
                    };
                    if sane_score(target) && g.set_f32(SCORE, target) {
                        s.say(format!("Score set to {target:.0}"));
                        score = target;
                    }
                }
                // Score multiplier: the factor inside the bank stub (1.0 = off).
                if bank_factor.is_none() {
                    bank_factor = Some(g.bank_stub());
                    if bank_factor == Some(None) {
                        s.say("Score multiplier: unexpected code, not available");
                    }
                }
                if let Some(Some(addr)) = bank_factor {
                    let want = if cheats.multiplier_on { cheats.multiplier } else { 1.0 };
                    if g.f32(addr) != Some(want) {
                        g.set_f32(addr, want);
                    }
                }
                let in_combo = (1..10_000).contains(&combo);
                if cheats.endless_combo && in_combo {
                    g.set_f32(COMBO_TIMER, 1.5);
                }
                // Combo multiplier: hold it while a combo is building (base points > 0).
                if cheats.combo_mult_on && g.f32(COMBO_BASE).is_some_and(|b| b > 0.0) && mult.is_finite() {
                    g.set_f32(COMBO_MULT, cheats.combo_mult);
                }
                if rope_patched != Some(cheats.unbreakable_rope) {
                    let new: &[u8] = if cheats.unbreakable_rope { &NOP5 } else { &ROPE_RIP_ORIGINAL };
                    if g.patch_code(ROPE_RIP_CALL, &[&ROPE_RIP_ORIGINAL, &NOP5], new) {
                        rope_patched = Some(cheats.unbreakable_rope);
                    } else if cheats.unbreakable_rope {
                        s.say("Unbreakable rope: unexpected code, not applied");
                        s.cheats.unbreakable_rope = false;
                        rope_patched = Some(false);
                    }
                }
                if reach_patched.is_none() {
                    // Undo the v0.4.1 code patch if it is still in memory.
                    g.patch_code(GRAB_RANGE_JUMP, &[&GRAB_RANGE_ORIGINAL, &NOP6], &GRAB_RANGE_ORIGINAL);
                    reach_patched = Some(false);
                }
                reach.tick(g, cheats.grab_anywhere);
                if crash_patched != Some(cheats.no_crash) {
                    let on = cheats.no_crash;
                    let a = g.patch_code(START_CRASH, &[&[0x57], &RET], if on { &RET } else { &[0x57] });
                    let b = g.patch_code(CRASH_BOUNCE, &[&[0x83], &RET], if on { &RET } else { &[0x83] });
                    if a && b {
                        crash_patched = Some(on);
                    } else if on {
                        s.say("No crashing: unexpected code, not applied");
                        s.cheats.no_crash = false;
                    } else {
                        crash_patched = Some(false);
                    }
                }
                if levels_patched != Some(cheats.hidden_levels) {
                    let on = cheats.hidden_levels;
                    let mut ok = true;
                    for site in HIDDEN_LEVEL_CHECKS {
                        for (offset, level) in [(2u32, 0x04u8), (7, 0x08)] {
                            let new = if on { 0xFF } else { level };
                            ok &= g.patch_code(site + offset, &[&[level], &[0xFF]], &[new]);
                        }
                    }
                    if ok {
                        levels_patched = Some(on);
                    } else if on {
                        s.say("Hidden levels: unexpected code, not applied");
                        s.cheats.hidden_levels = false;
                    } else {
                        levels_patched = Some(false);
                    }
                }
                speed.tick(g, &cheats);
                if jump.tick(g, &cheats) {
                    s.say("Jump!");
                }
                if s.game_jump_key != jump.game_key.flatten() {
                    s.game_jump_key = jump.game_key.flatten();
                    changed = true;
                }
                if s.score != score || s.combo != combo || s.combo_mult != mult {
                    (s.score, s.combo, s.combo_mult) = (score, combo, mult);
                    changed = true;
                }
            }
            if changed {
                ctx.request_repaint();
            }
        }
    });
}

/// "No speed loss" cheats: remember the rider's speed when a grind starts / the rope is let go, and
/// top the velocity back up to it whenever the game slows the rider down (direction is kept).
#[derive(Default)]
struct SpeedLock {
    was_grinding: bool,
    was_holding: bool,
    prev_speed: f32,
    prev_flat: f32,
    grind_target: f32,
    rope_target: f32,
}

impl SpeedLock {
    fn tick(&mut self, g: &Process, c: &Cheats) {
        let Some(block) = g.u32(PLAYER_BLOCK_PTR).filter(|&b| b != 0) else { return };
        let (Some(mode), Some(w0), Some(w2), Some(v)) =
            (g.i32(block + PHYS_MODE), g.u32(block + STATE_W0), g.u32(block + STATE_W2), g.vec3(block + VELOCITY))
        else {
            return;
        };
        if !v.iter().all(|c| c.is_finite()) {
            return;
        }
        let grinding = mode == 1 || w0 & RAIL_RIDE != 0;
        let holding = w2 & HOLDING_ROPE != 0;
        // On a rail (mode 1) the game stores (speed along the rail, vertical, -) instead of a world vector.
        let on_rail = mode == 1;
        let speed = if on_rail { v[0].abs() } else { (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() };
        let flat = if on_rail { speed } else { (v[0] * v[0] + v[2] * v[2]).sqrt() }; // Y is up in game space
        let mut out = v;

        if grinding {
            if !self.was_grinding {
                self.grind_target = self.prev_speed.max(speed);
            }
            self.grind_target = self.grind_target.max(speed);
            if c.grind_speed && speed > 0.05 && speed < self.grind_target {
                if on_rail {
                    out[0] = self.grind_target.copysign(v[0]);
                } else {
                    out = v.map(|x| x * self.grind_target / speed);
                }
            }
        } else if !holding {
            if self.was_holding || self.was_grinding {
                self.rope_target = self.prev_flat.max(flat);
            }
            self.rope_target = self.rope_target.max(flat);
            if c.rope_off_speed && flat > 0.05 && flat < self.rope_target {
                let k = self.rope_target / flat;
                out = [v[0] * k, v[1], v[2] * k];
            }
        } else {
            self.rope_target = 0.0;
        }
        if out != v {
            g.set_vec3(block + VELOCITY, out);
        }
        self.was_grinding = grinding;
        self.was_holding = holding;
        self.prev_speed = speed;
        self.prev_flat = flat;
    }
}

/// Infinite rope grab range by stretching the rope: see GRAB_WINDOW / HANDLE_LEN above.
#[derive(Default)]
struct RopeReach {
    block: u32,
    normal_len: Option<f32>, // the level's handle length, read while we had not changed it
    stretched: bool,
    last: Option<Instant>,
}

impl RopeReach {
    fn tick(&mut self, g: &Process, on: bool) {
        let now = Instant::now();
        let dt = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f32()).min(0.1);
        self.last = Some(now);
        let Some(block) = g.u32(PLAYER_BLOCK_PTR).filter(|&b| b != 0) else { return };
        if block != self.block {
            *self = RopeReach { block, last: Some(now), ..Default::default() };
        }
        let (Some(state), Some(window), Some(len), Some(dist)) =
            (g.i32(block + ROPE_STATE), g.f32(block + GRAB_WINDOW), g.f32(block + HANDLE_LEN), g.f32(ROPE_DISTANCE))
        else {
            return;
        };
        if !self.stretched {
            if len.is_finite() && (0.0..50.0).contains(&len) {
                self.normal_len = Some(len); // learn (or re-learn after a level load) the real length
            }
        }
        let Some(normal) = self.normal_len else { return };
        let extra = g
            .u32(block + LEVEL_ROPE_TABLE)
            .filter(|&t| t != 0)
            .zip(g.i32(block + LEVEL_ROPE_INDEX).filter(|i| (0..64).contains(i)))
            .and_then(|(t, i)| g.f32(t + 0x3ef0 + i as u32 * 4))
            .filter(|e| e.is_finite())
            .unwrap_or(0.0);
        // Length that keeps the rider inside reach, with a little slack.
        let needed = (dist - extra + 1.0).max(normal);
        let mut want = len;
        if on && window > 0.0 && dist.is_finite() {
            want = len.max(needed); // calling for / just caught the rope: cover the distance
        } else if self.stretched && state == 0 {
            want = (len - REEL_SPEED * dt).max(normal); // holding it: reel back to normal
        } else if self.stretched && state != 3 {
            want = normal; // dropped again or cheat off: back to normal
        }
        if !on && self.stretched && state != 0 {
            want = normal;
        }
        if (want - len).abs() > 1e-4 {
            g.set_f32(block + HANDLE_LEN, want);
        }
        self.stretched = want > normal + 1e-3;
    }
}

/// Double jump anywhere: on the jump key, push the rider's vertical speed up to `jump_strength`.
/// From the water this is a jump anywhere; in the air it is the double jump (one per airtime, reset
/// when the rider touches the water or ground). Only while the game window is in front, never on rails.
#[derive(Default)]
struct DoubleJump {
    was_down: bool,
    air_jumps: u32,
    game_key: Option<Option<i32>>, // config.cfg jump key, read once per attach
}

impl DoubleJump {
    fn tick(&mut self, g: &Process, c: &Cheats) -> bool {
        let game_key = *self.game_key.get_or_insert_with(|| game_jump_vk(&g.dir));
        let key = if c.jump_key == 0 { game_key.unwrap_or(0x20) } else { c.jump_key };
        let down = unsafe { GetAsyncKeyState(key) } as u16 & 0x8000 != 0;
        let pressed = down && !self.was_down;
        self.was_down = down;
        let Some(block) = g.u32(PLAYER_BLOCK_PTR).filter(|&b| b != 0) else { return false };
        let (Some(mode), Some(w0)) = (g.i32(block + PHYS_MODE), g.u32(block + STATE_W0)) else { return false };
        let airborne = w0 & AIRBORNE != 0;
        if !airborne && w0 & SURFACE_CONTACT != 0 {
            self.air_jumps = 0;
        }
        if !c.double_jump || !pressed || mode == 1 || w0 & RAIL_RIDE != 0 || !game_in_front(g.pid) {
            return false;
        }
        if airborne && !c.unlimited_jumps && self.air_jumps >= 1 {
            return false;
        }
        let Some(mut v) = g.vec3(block + VELOCITY).filter(|v| v.iter().all(|c| c.is_finite())) else { return false };
        v[1] = v[1].max(c.jump_strength); // Y is up
        if !g.set_vec3(block + VELOCITY, v) {
            return false;
        }
        if airborne {
            self.air_jumps += 1;
        }
        true
    }
}

fn game_in_front(pid: u32) -> bool {
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut owner) };
    owner == pid
}
