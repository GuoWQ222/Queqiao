//! An HTTP(S) GET on Linux, with curl (docs/LINUX.md; the package depends on it): the system's certificates and the
//! person's proxy settings (its environment), limited in size and time, redirects followed but never to anything but
//! HTTPS. As WinHTTP does on Windows.

use std::process::{Command, Stdio};

/// The body of `url` (status 200 only, at most `limit` bytes, all of it within `within`).
pub fn fetch(url: &str, limit: u64, within: std::time::Duration) -> Result<Vec<u8>, String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://"))
        || url.contains(char::is_whitespace)
    {
        return Err(format!("not an http(s) address: {url}"));
    }
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-redirs",
            "5",
            // The address itself as given (the feed check allows HTTP to this PC only); redirects to HTTPS only.
            "--proto",
            "=https,http",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "20",
        ])
        .arg("--max-filesize")
        .arg(limit.to_string())
        .arg("--max-time")
        .arg(within.as_secs().max(1).to_string())
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{url}: curl: {e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{url}: {}", error.trim()));
    }
    if output.stdout.len() as u64 > limit {
        return Err(format!("{url}: more than {limit} bytes"));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_fetched() {
        let within = std::time::Duration::from_secs(5);
        assert!(fetch("file:///etc/passwd", 100, within).is_err());
        assert!(fetch("http://127.0.0.1:9/ -o /tmp/x", 100, within).is_err());
        // Nothing listens there: an error, not a hang.
        assert!(fetch("http://127.0.0.1:9/queqiao", 100, within).is_err());
    }
}
