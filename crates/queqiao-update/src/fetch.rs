//! An HTTP(S) GET with WinHTTP: the system's proxy settings and certificates, limited in size and time.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{PCWSTR, w};

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Address {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse(url: &str) -> Result<Address, String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("not an http(s) address: {url}"));
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse().map_err(|_| format!("bad port in {url}"))?,
        ),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() || authority.contains('@') {
        return Err(format!("bad host in {url}"));
    }
    Ok(Address {
        secure,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// The body of `url` (status 200 only, at most `limit` bytes, all of it within `within`).
pub fn fetch(url: &str, limit: u64, within: std::time::Duration) -> Result<Vec<u8>, String> {
    let started = std::time::Instant::now();
    let address = parse(url)?;
    let failed = |what: &str| {
        format!(
            "{url}: {what}: {}",
            windows::core::Error::from_thread().message()
        )
    };
    unsafe {
        let session = Handle(WinHttpOpen(
            w!("Queqiao"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(failed("WinHttpOpen"));
        }
        // Resolving, connecting, sending: 15 s each; receiving: 60 s between reads.
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 60_000)
            .map_err(|_| failed("timeouts"))?;
        let host = wide(&address.host);
        let connection = Handle(WinHttpConnect(
            session.0,
            PCWSTR(host.as_ptr()),
            address.port,
            0,
        ));
        if connection.0.is_null() {
            return Err(failed("connect"));
        }
        let path = wide(&address.path);
        let flags = if address.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if request.0.is_null() {
            return Err(failed("request"));
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|_| failed("send"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| failed("response"))?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|_| failed("status"))?;
        if status != 200 {
            return Err(format!("{url}: HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
            .map_err(|_| failed("read"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..read as usize]);
            if body.len() as u64 > limit {
                return Err(format!("{url}: larger than {limit} bytes"));
            }
            if started.elapsed() > within {
                return Err(format!("{url}: not done within {} s", within.as_secs()));
            }
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a =
            parse("https://github.com/x/y/releases/latest/download/queqiao-update.json").unwrap();
        assert!(a.secure && a.host == "github.com" && a.port == 443);
        assert_eq!(a.path, "/x/y/releases/latest/download/queqiao-update.json");
        let b = parse("http://127.0.0.1:8123/feed/x").unwrap();
        assert!(!b.secure && b.host == "127.0.0.1" && b.port == 8123 && b.path == "/feed/x");
        assert!(parse("ftp://x/").is_err());
        assert!(parse("https://user@host/").is_err());
        assert!(parse("https://:80/").is_err());
    }
}
