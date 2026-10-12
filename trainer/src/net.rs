//! Minimal HTTPS GET on Windows' own WinHTTP (system proxy settings, no extra TLS library).
//! Redirects (GitHub release downloads go to a CDN) are followed by WinHTTP itself.
use std::ffi::c_void;

use windows_sys::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
};

const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const FLAG_SECURE: u32 = 0x0080_0000;
const ADDREQ_FLAG_ADD: u32 = 0x2000_0000;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_CONTENT_LENGTH: u32 = 5;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn last_error(what: &str) -> String {
    format!("{what} failed ({})", std::io::Error::last_os_error())
}

/// GET an https:// URL. `progress(done, total)` is called as data arrives (total 0 if unknown).
pub fn get(url: &str, accept: &str, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("only https URLs are supported")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let agent = wide(concat!("wbu-trainer/", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = Handle(WinHttpOpen(agent.as_ptr(), ACCESS_TYPE_AUTOMATIC_PROXY as _, std::ptr::null(), std::ptr::null(), 0));
        if session.0.is_null() {
            return Err(last_error("WinHttpOpen"));
        }
        let connect = Handle(WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0));
        if connect.0.is_null() {
            return Err(last_error("connect"));
        }
        let verb = wide("GET");
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            wide(&path).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            FLAG_SECURE as _,
        ));
        if request.0.is_null() {
            return Err(last_error("request"));
        }
        let headers = wide(&format!("Accept: {accept}\r\n"));
        WinHttpAddRequestHeaders(request.0, headers.as_ptr(), u32::MAX, ADDREQ_FLAG_ADD as _);
        if WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0
        {
            return Err(last_error("download"));
        }
        let number = |info: u32| {
            let (mut v, mut len) = (0u32, 4u32);
            let ok = WinHttpQueryHeaders(
                request.0,
                info | QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut v as *mut u32).cast(),
                &mut len,
                std::ptr::null_mut(),
            );
            (ok != 0).then_some(v)
        };
        let status = number(QUERY_STATUS_CODE).unwrap_or(0);
        if status != 200 {
            return Err(format!("server answered HTTP {status}"));
        }
        let total = number(QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;
        let mut body = Vec::with_capacity(total as usize);
        loop {
            let mut avail = 0u32;
            if WinHttpQueryDataAvailable(request.0, &mut avail) == 0 {
                return Err(last_error("read"));
            }
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut got = 0u32;
            if WinHttpReadData(request.0, body[start..].as_mut_ptr().cast(), avail, &mut got) == 0 {
                return Err(last_error("read"));
            }
            body.truncate(start + got as usize);
            progress(body.len() as u64, total);
        }
        Ok(body)
    }
}
