//! WBU trainer helper: a dinput8.dll proxy loaded by Wakeboarding Unleashed (PC, 2003).
//!
//! Forwards DirectInput8Create to the system DLL, logs the files the game opens (with optional
//! folder redirects), and runs the live model changes (see live.rs). Coexists with the widescreen
//! fix (d3d8.dll).

#![allow(non_snake_case)]

mod live;
mod mods;

use std::ffi::{CStr, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::CreateFileA;
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

// ------------------------------------------------------------------ WBU.EXE addresses (image base 0x400000)

const IAT_CREATE_FILE_A: usize = 0x59e030; // import slot for KERNEL32!CreateFileA
/// Shrinks a loaded model file in place and aborts with "realloc relocated memory block!" when the
/// heap moves it, which modern Windows may do. Skipping the shrink is safe: the block is freed whole later.
const SHRINK_LOADED_FILE: usize = 0x4eb260;
const SHRINK_PROLOGUE: [u8; 8] = [0x8b, 0x44, 0x24, 0x04, 0x8b, 0x4c, 0x24, 0x08];
/// Strings that identify the analysed build and that no other mod patches.
const SIGNATURES: [(usize, &[u8]); 2] = [(0x5c1440, b"WBU CD2\0"), (0x5c5674, b"Save\\%s %d.CAR\0")];

// ------------------------------------------------------------------ log

pub(crate) static DIR: OnceLock<PathBuf> = OnceLock::new();
static LOG: Mutex<()> = Mutex::new(());
static START: OnceLock<Instant> = OnceLock::new();

pub(crate) fn log(msg: impl AsRef<str>) {
    let _g = LOG.lock();
    if let Some(dir) = DIR.get()
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join("wbu-trainer.log"))
    {
        let t = START.get().map_or(0.0, |s| s.elapsed().as_secs_f32());
        let _ = writeln!(f, "[{t:8.2}] {}", msg.as_ref());
    }
}

pub(crate) unsafe fn write_protected<T: Copy>(addr: usize, value: T) {
    unsafe {
        let mut old = 0;
        VirtualProtect(addr as *const c_void, size_of::<T>(), PAGE_EXECUTE_READWRITE, &mut old);
        (addr as *mut T).write_unaligned(value);
        VirtualProtect(addr as *const c_void, size_of::<T>(), old, &mut old);
    }
}

fn exe_matches() -> bool {
    SIGNATURES
        .iter()
        .all(|(addr, bytes)| unsafe { std::slice::from_raw_parts(*addr as *const u8, bytes.len()) } == *bytes)
}

// ------------------------------------------------------------------ file opens

static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Folder redirects from wbu-trainer.ini, e.g. `data\levels\Florida = data\levels\Secret`.
/// Keys are normalised (lower case, no spaces before separators) because the game pads folder
/// names with a trailing space ("data\players\shaun \player.bin"). Re-read whenever the ini
/// changes, so a redirect applies to the next time the game opens those files.
struct Redirects {
    rules: Vec<(String, String)>,
    stamp: Option<std::time::SystemTime>,
    next_check: Option<Instant>,
}
static REDIRECTS: Mutex<Redirects> = Mutex::new(Redirects { rules: Vec::new(), stamp: None, next_check: None });

fn refresh_redirects(r: &mut Redirects) {
    let now = Instant::now();
    if r.next_check.is_some_and(|t| now < t) {
        return;
    }
    r.next_check = Some(now + std::time::Duration::from_millis(250));
    let Some(dir) = DIR.get() else { return };
    let stamp = std::fs::metadata(dir.join("wbu-trainer.ini")).and_then(|m| m.modified()).ok();
    if stamp == r.stamp {
        return;
    }
    r.stamp = stamp;
    let rules = load_redirects(dir);
    if rules != r.rules {
        if rules.is_empty() {
            log("redirects: none");
        }
        for (from, to) in &rules {
            log(format!("redirect {from} -> {to}"));
        }
        r.rules = rules;
    }
}

pub(crate) fn normalise(path: &str) -> String {
    let mut out = path.replace('/', "\\").to_lowercase();
    while out.contains(" \\") {
        out = out.replace(" \\", "\\");
    }
    out.trim_end().to_string()
}

fn load_redirects(dir: &std::path::Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(dir.join("wbu-trainer.ini")).unwrap_or_default();
    let mut rules = Vec::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_lowercase();
        } else if section == "redirect" && !line.starts_with(';') {
            if let Some((from, to)) = line.split_once('=') {
                let (from, to) = (normalise(from.trim()), to.trim().trim_end_matches(['\\', '/']).to_string());
                if !from.is_empty() && !to.is_empty() {
                    rules.push((from.trim_end_matches('\\').to_string(), to));
                }
            }
        }
    }
    rules
}

/// Apply the first matching folder redirect; returns the new path (NUL-terminated) if any.
fn redirect(path: &str) -> Option<Vec<u8>> {
    let norm = normalise(path);
    let mut r = REDIRECTS.lock().ok()?;
    refresh_redirects(&mut r);
    for (from, to) in &r.rules {
        if let Some(rest) = norm.strip_prefix(from.as_str())
            && (rest.is_empty() || rest.starts_with('\\'))
        {
            let mut out = format!("{to}{rest}").into_bytes();
            out.push(0);
            return Some(out);
        }
    }
    None
}

unsafe extern "system" fn hook_create_file(
    name: *const u8,
    access: u32,
    share: u32,
    security: *const SECURITY_ATTRIBUTES,
    disposition: u32,
    flags: u32,
    template: HANDLE,
) -> HANDLE {
    let mut redirected = None;
    if !name.is_null() {
        let path = unsafe { CStr::from_ptr(name.cast()) }.to_string_lossy().into_owned();
        redirected = redirect(&path);
        let mut seen = SEEN.lock().unwrap();
        // Every open is logged once; a level's geometry every time, to see when levels really load.
        let level_geometry = normalise(&path).ends_with("\\track.nsr") && !path.contains("PC\\");
        if level_geometry || !seen.contains(&path) {
            match &redirected {
                Some(r) => log(format!("open {path} -> {}", String::from_utf8_lossy(&r[..r.len() - 1]))),
                None => log(format!("open {path}")),
            }
            seen.push(path);
        }
    }
    let name = redirected.as_ref().map_or(name, |r| r.as_ptr());
    unsafe { CreateFileA(name, access, share, security, disposition, flags, template) }
}

// ------------------------------------------------------------------ DirectInput forwarding

static REAL_DI8_CREATE: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DirectInput8Create(
    instance: *mut c_void,
    version: u32,
    riid: *const c_void,
    out: *mut *mut c_void,
    outer: *mut c_void,
) -> i32 {
    unsafe {
        if REAL_DI8_CREATE.load(Relaxed) == 0 {
            let mut buf = [0u16; 260];
            let n = GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) as usize;
            let path: Vec<u16> = buf[..n].iter().copied().chain("\\dinput8.dll\0".encode_utf16()).collect();
            match GetProcAddress(LoadLibraryW(path.as_ptr()), c"DirectInput8Create".as_ptr().cast()) {
                Some(f) => REAL_DI8_CREATE.store(f as usize, Relaxed),
                None => {
                    log("FATAL: could not load the system dinput8.dll");
                    return 0x8000_4005u32 as i32; // E_FAIL
                }
            }
        }
        let real: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(REAL_DI8_CREATE.load(Relaxed));
        real(instance, version, riid, out, outer)
    }
}

// ------------------------------------------------------------------ entry point

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason != DLL_PROCESS_ATTACH {
        return 1;
    }
    unsafe {
        let _ = START.set(Instant::now());
        let mut buf = [0u16; 520];
        let n = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        let dll = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        let dir = dll.parent().map(PathBuf::from).unwrap_or_default();
        let _ = DIR.set(dir.clone());
        let _ = std::fs::remove_file(dir.join("wbu-trainer.log"));
        log("wbu-trainer helper loaded");
        if !exe_matches() {
            log("WBU.EXE does not match the expected build; helper disabled");
            return 1;
        }
        if let Ok(mut r) = REDIRECTS.lock() {
            refresh_redirects(&mut r);
        }
        let prologue = std::slice::from_raw_parts(SHRINK_LOADED_FILE as *const u8, SHRINK_PROLOGUE.len());
        if prologue == SHRINK_PROLOGUE {
            write_protected(SHRINK_LOADED_FILE, 0xC3u8); // ret (cdecl: the caller pops the arguments)
            log("fixed: 'realloc relocated memory block' crash on level change / quit");
        }
        live::install();
        write_protected(IAT_CREATE_FILE_A, hook_create_file as *const () as usize);
        log("watching file opens");
    }
    1
}
