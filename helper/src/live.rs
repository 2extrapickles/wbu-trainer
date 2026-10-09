//! Live model changes, applied while the game runs (no restart).
//!
//! The game builds every model ("skin") through one function, 0x544680(sqx, "data\<folder>\textures.nsr"),
//! and draws them all from its render queue through one call, 0x544df0 (this = skin). Skins are skinned
//! on the CPU every frame from the vertex array at skin[5] (the SQX data in the loaded file).
//!
//! * The create hook records each skin's folder and a pristine copy of its SQX blob.
//! * The draw hook (on the game thread) re-reads wbu-trainer.ini when it changes, then:
//!   - swaps the skin being drawn for a replacement built from any model file (created on demand
//!     with the game's own loader, so textures and vertex buffers are the game's),
//!   - rewrites vertex positions for big heads / boat size (only on the skin being drawn, which is
//!     therefore alive).
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use crate::{log, mods};

const CREATE_SKIN: usize = 0x544680; // cdecl skin* (sqx*, const char* texture_path)
const CREATE_THUNK: usize = 0x5456e0; // jmp CREATE_SKIN; every caller goes through it
const DRAW_SKIN: usize = 0x544df0; // thiscall (skin, 6 stack args), ret 0x18
const DRAW_CALL_SITE: usize = 0x545670; // call DRAW_SKIN in the render queue flush

type CreateFn = unsafe extern "C" fn(*mut u8, *const u8) -> *mut c_void;
type DrawFn = unsafe extern "thiscall" fn(*mut c_void, u32, u32, u32, u32, u32, u32);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Rider,
    Boat,
    Other,
}

fn kind_of(folder: &str) -> Kind {
    if folder.starts_with("players\\") {
        Kind::Rider
    } else if folder.starts_with("boats\\") {
        Kind::Boat
    } else {
        Kind::Other
    }
}

struct Skin {
    folder: String,
    /// What the skin stands in for: its own kind, or the kind it replaces.
    role: Kind,
    /// True when the mesh has rider bones (big heads apply).
    rider_mesh: bool,
    /// Rigid stand-in (rider size applies), scaled around `anchor`.
    rigid: bool,
    anchor: [f32; 3],
    pristine: Vec<u8>,
    /// Vertex edit currently in game memory: (big head, scale); (1, 1) = pristine.
    applied: (f32, f32),
}

#[derive(Clone, PartialEq, Default)]
struct Look {
    big_head: f32,
    boat_scale: f32,
    rider: String,
    rider_size: f32,
    rider_turn: u32,
    boat: String,
    boat_turn: u32,
}

struct State {
    skins: HashMap<usize, Skin>,
    /// Replacement skins by "original folder|look key".
    replacements: HashMap<String, usize>,
    look: Look,
    ini_stamp: Option<SystemTime>,
    next_check: Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn normalise_folder(path: &str) -> String {
    let n = crate::normalise(path);
    let n = n.strip_prefix("data\\").unwrap_or(&n);
    n.rsplit_once('\\').map_or(n.to_string(), |(dir, _)| dir.to_string())
}

// ------------------------------------------------------------------ settings

fn parse_look(text: &str) -> Look {
    let mut look = Look { big_head: 1.0, boat_scale: 1.0, rider_size: 1.0, ..Default::default() };
    for line in text.lines().map(str::trim) {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim().to_lowercase(), v.trim());
        let num = |lo: f32, hi: f32| v.parse::<f32>().ok().filter(|x| x.is_finite()).map(|x| x.clamp(lo, hi));
        match k.as_str() {
            "big_head" => look.big_head = num(0.2, 5.0).unwrap_or(1.0),
            "boat_scale" => look.boat_scale = num(0.1, 5.0).unwrap_or(1.0),
            "rider" => look.rider = crate::normalise(v),
            "rider_size" => look.rider_size = num(0.05, 10.0).unwrap_or(1.0),
            "rider_turn" => look.rider_turn = v.parse::<u32>().unwrap_or(0) % 4,
            "boat" => look.boat = crate::normalise(v),
            "boat_turn" => look.boat_turn = v.parse::<u32>().unwrap_or(0) % 4,
            _ => {}
        }
    }
    look
}

impl State {
    fn reload_if_changed(&mut self) {
        if Instant::now() < self.next_check {
            return;
        }
        self.next_check = Instant::now() + std::time::Duration::from_millis(300);
        let Some(dir) = crate::DIR.get() else { return };
        let path = dir.join("wbu-trainer.ini");
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if stamp == self.ini_stamp {
            return;
        }
        self.ini_stamp = stamp;
        let look = parse_look(&std::fs::read_to_string(&path).unwrap_or_default());
        if look != self.look {
            log(format!(
                "look: big_head x{} boat x{} rider '{}' x{} turn {} boat '{}' turn {}",
                look.big_head, look.boat_scale, look.rider, look.rider_size, look.rider_turn, look.boat, look.boat_turn
            ));
            self.look = look;
        }
    }

    /// The skin to draw instead of `skin`, building it on first use. None = draw the original.
    fn replacement_for(&mut self, skin: usize) -> Option<usize> {
        let orig = self.skins.get(&skin)?;
        let folder = orig.folder.clone();
        let (target, turn) = match orig.role {
            Kind::Rider if !self.look.rider.is_empty() => (self.look.rider.clone(), self.look.rider_turn),
            Kind::Boat if !self.look.boat.is_empty() => (self.look.boat.clone(), self.look.boat_turn),
            _ => return None,
        };
        if target == folder {
            return None;
        }
        let key = format!("{folder}|{target}|{turn}");
        if let Some(&r) = self.replacements.get(&key) {
            return (r != 0).then_some(r);
        }
        let built = self.build(skin, &target, turn);
        if let Err(e) = &built {
            log(format!("replacement {target} for {folder}: {e}"));
        }
        let r = built.unwrap_or(0);
        self.replacements.insert(key, r);
        (r != 0).then_some(r)
    }

    fn build(&mut self, skin: usize, target: &str, turn: u32) -> Result<usize, String> {
        let orig = &self.skins[&skin];
        let file = ["player.bin", "boat.bin", "object.bin"]
            .iter()
            .find_map(|f| std::fs::read(format!("data\\{target}\\{f}")).ok())
            .ok_or("no model file in that folder")?;
        let mut blob = mods::extract(&file).ok_or("no SQX mesh in the file")?;
        let l = mods::layout(&blob).unwrap();
        let ol = mods::layout(&orig.pristine).unwrap();
        let target_kind = kind_of(target);
        let rider_mesh = orig.role == Kind::Rider && target_kind == Kind::Rider && orig.rider_mesh;
        let info;
        if rider_mesh {
            // Rider for rider: every rider has its own bone numbering, so map each bone to the
            // original rider's bone whose vertices sit closest in the (shared) T-pose.
            let mine = mods::bone_centroids(&blob, &l);
            let theirs = mods::bone_centroids(&orig.pristine, &ol);
            let map: HashMap<u8, u8> = mine
                .iter()
                .map(|(b, c, _)| {
                    let near = theirs
                        .iter()
                        .min_by(|x, y| dist(x.1, *c).total_cmp(&dist(y.1, *c)))
                        .map_or(*b, |t| t.0);
                    (*b, near)
                })
                .collect();
            mods::remap_bones(&mut blob, &l, |b| *map.get(&b).unwrap_or(&b));
            if turn != 0 {
                mods::transform(&mut blob, &l, turn, 1.0, [0.0; 3]);
            }
            info = format!("{} bones matched", map.len());
        } else {
            // Anything else rides rigidly on the original's board / hull bone, fitted to its size:
            // longest sides aligned, same length, standing on the same ground.
            let bone = mods::attach_bone(&orig.pristine, &ol);
            mods::make_rigid(&mut blob, &l, bone);
            mods::clear_variants(&mut blob, &l);
            let (olo, ohi) = mods::bbox(&orig.pristine, &ol);
            let (lo, hi) = mods::bbox(&blob, &l);
            let along_x = |lo: [f32; 3], hi: [f32; 3]| hi[0] - lo[0] >= hi[2] - lo[2];
            let auto = if along_x(lo, hi) != along_x(olo, ohi) { 1 } else { 0 };
            let longest = |lo: [f32; 3], hi: [f32; 3]| (0..3).map(|k| hi[k] - lo[k]).fold(0.0f32, f32::max);
            let s = longest(olo, ohi) / longest(lo, hi).max(1e-3);
            mods::transform(&mut blob, &l, auto + turn, s, [0.0; 3]);
            let (lo, hi) = mods::bbox(&blob, &l);
            let t = [(olo[0] + ohi[0] - lo[0] - hi[0]) / 2.0, olo[1] - lo[1], (olo[2] + ohi[2] - lo[2] - hi[2]) / 2.0];
            mods::transform(&mut blob, &l, 0, 1.0, t);
            info = format!("rigid on bone {bone}, scale {s:.2}");
        }
        let (lo, hi) = mods::bbox(&blob, &l);
        let anchor = [(lo[0] + hi[0]) / 2.0, lo[1], (lo[2] + hi[2]) / 2.0];
        let rigid = !rider_mesh;
        let pristine = blob.clone();
        // The game keeps pointers into the blob for the life of the skin: leak it on purpose.
        let data: &'static mut [u8] = Box::leak(blob.into_boxed_slice());
        let tex = format!("data\\{target}\\textures.nsr\0");
        let create: CreateFn = unsafe { std::mem::transmute(CREATE_SKIN) };
        let new = unsafe { create(data.as_mut_ptr(), tex.as_ptr()) } as usize;
        if new == 0 {
            return Err("the game could not build it".into());
        }
        log(format!("built {target} for {} ({info})", orig.folder));
        let role = orig.role;
        self.skins.insert(new, Skin { folder: target.to_string(), role, rider_mesh, rigid, anchor, pristine, applied: (1.0, 1.0) });
        Ok(new)
    }

    /// Bring the vertex edit of a skin (being drawn, so alive) up to date.
    fn apply_edit(&mut self, skin: usize) {
        let Some(s) = self.skins.get_mut(&skin) else { return };
        let want = match s.role {
            Kind::Rider => (if s.rider_mesh { self.look.big_head } else { 1.0 }, if s.rigid { self.look.rider_size } else { 1.0 }),
            Kind::Boat => (1.0, self.look.boat_scale),
            Kind::Other => (1.0, 1.0),
        };
        if want == s.applied {
            return;
        }
        let (head, scale) = want;
        let l = mods::layout(&s.pristine).unwrap();
        let mut positions: Vec<[f32; 3]> = if head != 1.0 {
            mods::big_head(&s.pristine, head).unwrap_or_default()
        } else {
            Vec::new()
        };
        if positions.len() != l.nvert {
            positions = (0..l.nvert).map(|i| mods::pos(&s.pristine, &l, i)).collect();
        }
        let a = s.anchor;
        let around = |p: [f32; 3]| [0, 1, 2].map(|k| a[k] + (p[k] - a[k]) * scale);
        if scale != 1.0 {
            positions.iter_mut().for_each(|p| *p = around(*p));
        }
        unsafe {
            let words = skin as *const u32;
            let header = *words as *mut u8;
            let verts = *words.add(5) as *mut u8;
            if header.is_null() || verts.is_null() || *(header.add(0x1c) as *const u16) as usize != positions.len() {
                s.applied = want;
                return;
            }
            for (i, p) in positions.iter().enumerate() {
                let raw = mods::encode(*p);
                std::ptr::copy_nonoverlapping(raw.as_ptr() as *const u8, verts.add(i * 16), 6);
            }
            // Culling sphere: follows the scale, never smaller than the pristine one.
            let pr = |o: usize| f32::from_le_bytes(s.pristine[o..o + 4].try_into().unwrap());
            let c = around([pr(4), pr(8), pr(12)]);
            for (k, v) in c.iter().enumerate() {
                (header.add(4 + k * 4) as *mut f32).write_unaligned(*v);
            }
            (header.add(0x10) as *mut f32).write_unaligned(pr(0x10) * head.max(scale).max(1.0));
        }
        s.applied = want;
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

// ------------------------------------------------------------------ hooks

unsafe extern "C" fn hook_create(sqx: *mut u8, tex: *const u8) -> *mut c_void {
    // Copy the mesh before the game's loader rewrites parts of it in place.
    let pristine = unsafe {
        let header = std::slice::from_raw_parts(sqx, 0x38);
        mods::layout(header).map(|l| std::slice::from_raw_parts(sqx, l.size).to_vec())
    };
    let create: CreateFn = unsafe { std::mem::transmute(CREATE_SKIN) };
    let skin = unsafe { create(sqx, tex) };
    if !skin.is_null()
        && let Some(pristine) = pristine
    {
        let path = unsafe { std::ffi::CStr::from_ptr(tex.cast()) }.to_string_lossy().into_owned();
        let folder = normalise_folder(&path);
        let role = kind_of(&folder);
        let same = ["player.bin", "boat.bin", "object.bin"]
            .iter()
            .find_map(|f| std::fs::read(format!("data\\{folder}\\{f}")).ok())
            .and_then(|file| mods::extract(&file))
            .map(|blob| blob == pristine);
        log(format!("skin {:#x}: {folder} ({role:?}, {} bytes, matches file: {same:?})", skin as usize, pristine.len()));
        let mut guard = STATE.lock().unwrap();
        if let Some(st) = guard.as_mut() {
            // A new skin at an address we saw before replaces the stale entry (levels free theirs).
            let anchor = [0.0; 3]; // boats scale around their origin
            let skin_info = Skin { folder, role, rider_mesh: role == Kind::Rider, rigid: false, anchor, pristine, applied: (1.0, 1.0) };
            st.skins.insert(skin as usize, skin_info);
        }
    }
    skin
}

unsafe extern "thiscall" fn hook_draw(skin: *mut c_void, a: u32, b: u32, c: u32, d: u32, e: u32, f: u32) {
    let mut draw = skin as usize;
    if let Ok(mut guard) = STATE.lock()
        && let Some(st) = guard.as_mut()
    {
        st.reload_if_changed();
        if let Some(r) = st.replacement_for(draw) {
            draw = r;
        }
        st.apply_edit(draw);
    }
    let real: DrawFn = unsafe { std::mem::transmute(DRAW_SKIN) };
    unsafe { real(draw as *mut c_void, a, b, c, d, e, f) };
}

/// Patch a `call`/`jmp rel32` at `site` to go to `target`.
unsafe fn patch_rel32(site: usize, opcode: u8, target: usize) {
    unsafe {
        let rel = (target as isize - (site as isize + 5)) as i32;
        let mut bytes = [opcode, 0, 0, 0, 0];
        bytes[1..].copy_from_slice(&rel.to_le_bytes());
        crate::write_protected(site, bytes);
    }
}

pub fn install() -> bool {
    // Only patch the instructions we expect to find.
    let code = |a: usize, n: usize| unsafe { std::slice::from_raw_parts(a as *const u8, n) }.to_vec();
    let rel_target = |a: usize| {
        let b = code(a, 5);
        (a as isize + 5 + i32::from_le_bytes(b[1..5].try_into().unwrap()) as isize) as usize
    };
    if code(CREATE_THUNK, 1)[0] != 0xE9 || rel_target(CREATE_THUNK) != CREATE_SKIN {
        log("live models: create thunk not as expected, disabled");
        return false;
    }
    if code(DRAW_CALL_SITE, 1)[0] != 0xE8 || rel_target(DRAW_CALL_SITE) != DRAW_SKIN {
        log("live models: draw call not as expected, disabled");
        return false;
    }
    *STATE.lock().unwrap() = Some(State {
        skins: HashMap::new(),
        replacements: HashMap::new(),
        look: Look { big_head: 1.0, boat_scale: 1.0, rider_size: 1.0, ..Default::default() },
        ini_stamp: None,
        next_check: Instant::now(),
    });
    unsafe {
        patch_rel32(CREATE_THUNK, 0xE9, hook_create as *const () as usize);
        patch_rel32(DRAW_CALL_SITE, 0xE8, hook_draw as *const () as usize);
    }
    log("live models: hooks installed");
    true
}
