use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use serde_json::json;

pub struct PipManager {
    _child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl PipManager {
    pub fn new() -> Self {
        if std::env::var("CODEX_CU_PIP").ok().as_deref() == Some("false")
            || std::env::var("CODEX_CU_PIP").ok().as_deref() == Some("0")
        {
            return Self {
                _child: None,
                stdin: None,
            };
        }

        let bin = match find_or_build_pip_binary() {
            Some(path) => path,
            None => {
                return Self {
                    _child: None,
                    stdin: None,
                };
            }
        };

        let mut cmd = Command::new(bin);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(mut child) => {
                let stdin = child.stdin.take();
                Self {
                    _child: Some(child),
                    stdin,
                }
            }
            Err(_) => Self {
                _child: None,
                stdin: None,
            },
        }
    }

    pub fn send_target(&mut self, app_name: &str, bundle_id: &str, pid: i32) {
        let msg = json!({
            "event": "target",
            "appName": app_name,
            "bundleId": bundle_id,
            "pid": pid,
        });
        self.send_line(&msg);
    }

    pub fn send_frame(&mut self, base64_jpeg: &str) {
        let msg = json!({
            "event": "frame",
            "data": base64_jpeg,
        });
        self.send_line(&msg);
    }

    #[allow(dead_code)]
    pub fn send_hide(&mut self) {
        let msg = json!({ "event": "hide" });
        self.send_line(&msg);
    }

    fn send_line(&mut self, val: &serde_json::Value) {
        if let Some(stdin) = &mut self.stdin {
            if let Ok(mut bytes) = serde_json::to_vec(val) {
                bytes.push(b'\n');
                let _ = stdin.write_all(&bytes);
                let _ = stdin.flush();
            }
        }
    }
}

impl Drop for PipManager {
    fn drop(&mut self) {
        if let Some(stdin) = &mut self.stdin {
            let _ = stdin.write_all(b"{\"event\":\"quit\"}\n");
            let _ = stdin.flush();
        }
        if let Some(mut child) = self._child.take() {
            let _ = child.kill();
        }
    }
}

fn find_or_build_pip_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODEX_CU_PIP_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("codex-cu-pip");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }

    for candidate in [
        "target/release/codex-cu-pip",
        "target/debug/codex-cu-pip",
        "codex-cu-pip",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }

    // Try building on the fly if swift source exists
    let swift_src = Path::new("assets/pip.swift");
    if swift_src.is_file() {
        let out = std::env::temp_dir().join("codex-cu-pip");
        let status = Command::new("swiftc")
            .args(["-O", "assets/pip.swift", "-o"])
            .arg(&out)
            .status();
        if let Ok(s) = status {
            if s.success() && out.is_file() {
                return Some(out);
            }
        }
    }

    None
}

/// Parses target application details from raw child JSON-RPC output frames.
pub fn parse_app_activity(frame: &[u8]) -> Option<(String, String, i32)> {
    let s = std::str::from_utf8(frame).ok()?;

    // 1. "bundleID <bundle>, pid <pid>"
    if let Some(idx) = s.rfind("(bundleID ") {
        let rest = &s[idx + 10..];
        if let Some(end) = rest.find(')') {
            let inner = &rest[..end];
            if let Some((bundle, pid_str)) = inner.split_once(", pid ") {
                let pid = pid_str.trim().parse::<i32>().ok()?;
                let prefix = &s[..idx];
                let app_name = if let Some(app_idx) = prefix.rfind("App=") {
                    extract_app_name(&prefix[app_idx + 4..])
                } else {
                    bundle.to_string()
                };
                return Some((app_name, bundle.to_string(), pid));
            }
        }
    }

    // 2. "App=<name> (pid <pid>)"
    if let Some(idx) = s.rfind("(pid ") {
        let rest = &s[idx + 5..];
        if let Some(end) = rest.find(')') {
            let pid_str = &rest[..end];
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                let prefix = &s[..idx];
                if let Some(app_idx) = prefix.rfind("App=") {
                    let raw = &prefix[app_idx + 4..];
                    let name = extract_app_name(raw);
                    let bundle = if raw.contains('.') && !raw.contains('/') {
                        raw.trim().to_string()
                    } else {
                        String::new()
                    };
                    return Some((name, bundle, pid));
                }
            }
        }
    }

    // 3. "Window: \"...\", App: <name>."
    if let Some(idx) = s.rfind("\", App: ") {
        let rest = &s[idx + 8..];
        let name_end = rest
            .find(".\\n")
            .or_else(|| rest.find(".\n"))
            .or_else(|| rest.find('.'))
            .unwrap_or(rest.len());
        let name = rest[..name_end].trim();
        if !name.is_empty() {
            return Some((name.to_string(), String::new(), 0));
        }
    }

    None
}

fn extract_app_name(raw: &str) -> String {
    let unescaped = raw.replace("\\/", "/");
    let trimmed = unescaped
        .trim()
        .trim_matches(|c| c == '\'' || c == '"' || c == ' ' || c == '\\');
    let no_slash = trimmed.trim_end_matches('/');
    let basename = no_slash.rsplit('/').next().unwrap_or(no_slash);
    basename.strip_suffix(".app").unwrap_or(basename).to_string()
}

/// Extracts base64 JPEG from MCP image content in frame if present.
pub fn extract_frame_image(frame: &[u8]) -> Option<String> {
    if !frame.windows(b"\"image/jpeg\"".len()).any(|w| w == b"\"image/jpeg\"") {
        return None;
    }
    let val = serde_json::from_slice::<serde_json::Value>(frame).ok()?;
    let content = val.pointer("/result/content")?.as_array()?;
    for item in content {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("image") {
            if let Some(data) = item.get("data").and_then(serde_json::Value::as_str) {
                return Some(data.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_with_bundle_and_pid() {
        let frame = br#"{"result":{"content":[{"type":"text","text":"App=\/Applications\/SSHYard.app\/ (bundleID com.sshyard.desktop, pid 82543)\nWindow: SSHYard"}]}}"#;
        let (app, bundle, pid) = parse_app_activity(frame).unwrap();
        assert_eq!(app, "SSHYard");
        assert_eq!(bundle, "com.sshyard.desktop");
        assert_eq!(pid, 82543);
    }

    #[test]
    fn parses_app_with_pid_only() {
        let frame = b"App=com.sshyard.desktop (pid 82543)\nDiff...";
        let (app, _bundle, pid) = parse_app_activity(frame).unwrap();
        assert_eq!(app, "com.sshyard.desktop");
        assert_eq!(pid, 82543);
    }

    #[test]
    fn parses_app_from_js_state() {
        let frame = br#"{"result":{"content":[{"type":"text","text":"Window: \"SSHYard\", App: SSHYard.\n0 x"}]}}"#;
        let (app, _, pid) = parse_app_activity(frame).unwrap();
        assert_eq!(app, "SSHYard");
        assert_eq!(pid, 0);
    }
}
