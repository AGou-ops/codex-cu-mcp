//! 本机 Codex Computer Use 运行时定位与环境构建。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const SHIM_SOURCE: &str = include_str!("../assets/shim.mjs");

const NATIVE_ENV_PASSTHROUGH: [&str; 11] = [
    "HOME",
    "LOGNAME",
    "PATH",
    "SHELL",
    "USER",
    "__CF_USER_TEXT_ENCODING",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TZ",
];

const CUSTOM_CA_ENV_KEYS: [&str; 11] = [
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "GIT_SSL_CAINFO",
    "CARGO_HTTP_CAINFO",
    "PIP_CERT",
    "BUNDLE_SSL_CA_CERT",
    "npm_config_cafile",
    "NPM_CONFIG_CAFILE",
];

const PROXY_ENV_KEYS: [&str; 8] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLocation {
    pub app_path: String,
    pub node: String,
    pub client: String,
    pub codex_home: String,
    pub node_module_dirs: Option<String>,
    pub codex_cli: Option<String>,
    pub app_version: Option<String>,
}

impl RuntimeLocation {
    pub fn native_client_env(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = NATIVE_ENV_PASSTHROUGH
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value)))
            .collect();

        for key in CUSTOM_CA_ENV_KEYS {
            if let Ok(value) = std::env::var(key) {
                if !value.is_empty() {
                    env.insert(key.to_string(), value);
                }
            }
        }

        for key in PROXY_ENV_KEYS {
            if let Ok(value) = std::env::var(key) {
                if !value.is_empty() {
                    env.insert(key.to_string(), value);
                }
            }
        }

        env.insert("CODEX_HOME".into(), self.codex_home.clone());
        if let Some(cli) = &self.codex_cli {
            env.insert("CODEX_CLI_PATH".into(), cli.clone());
        }
        env
    }
}

pub fn default_codex_home() -> PathBuf {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".codex")
}

pub fn default_state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".codex-cu")
}

pub fn ensure_shim() -> anyhow::Result<PathBuf> {
    let state_dir = default_state_dir();
    fs::create_dir_all(&state_dir).with_context(|| format!("无法创建目录 {}", state_dir.display()))?;
    let path = state_dir.join("shim.mjs");
    let unchanged = fs::read_to_string(&path)
        .map(|existing| existing == SHIM_SOURCE)
        .unwrap_or(false);
    if !unchanged {
        fs::write(&path, SHIM_SOURCE).with_context(|| format!("无法写入 {}", path.display()))?;
    }
    Ok(path)
}

fn candidate_app_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("CHATGPT_APP_PATH") {
        paths.push(PathBuf::from(path));
    }
    paths.push(PathBuf::from("/Applications/ChatGPT.app"));
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(home);
        paths.push(home_path.join("Applications/ChatGPT.app"));
        paths.push(home_path.join("Applications/Codex.app"));
    }
    if let Ok(output) = Command::new("mdfind")
        .arg("kMDItemCFBundleIdentifier == \"com.openai.chat\"")
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let line = line.trim();
                if line.ends_with(".app") {
                    paths.push(PathBuf::from(line));
                }
            }
        }
    }
    paths
}

fn read_bundle_version(app_path: &Path) -> Option<String> {
    let plist = app_path.join("Contents/Info.plist");
    let output = Command::new("plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-"])
        .stdin(Stdio::from(fs::File::open(plist).ok()?))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

pub fn locate_runtime() -> anyhow::Result<Option<RuntimeLocation>> {
    let codex_home = default_codex_home();
    let mut client_candidates = Vec::new();
    if let Ok(custom) = std::env::var("CODEX_CU_CLIENT") {
        client_candidates.push(PathBuf::from(custom));
    }
    client_candidates.extend([
        codex_home
            .join("computer-use/Codex Computer Use.app")
            .join("Contents/SharedSupport/SkyComputerUseClient.app")
            .join("Contents/MacOS/SkyComputerUseClient"),
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/cua_node/lib/node_modules/@oai/sky/Codex Computer Use.app/Contents/SharedSupport/SkyComputerUseClient.app/Contents/MacOS/SkyComputerUseClient"),
    ]);
    let client = client_candidates.into_iter().find(|p| p.is_file());
    let Some(client) = client else {
        return Ok(None);
    };

    for app_path in candidate_app_paths() {
        let resources = app_path.join("Contents/Resources");
        let node = resources.join("cua_node/bin/node");
        if !node.is_file() {
            continue;
        }

        let module_dirs = resources.join("cua_node/lib/node_modules");
        let codex_cli = resources
            .join("codex-cli/CodexCLI.app/Contents/MacOS/codex")
            .is_file()
            .then(|| {
                resources
                    .join("codex-cli/CodexCLI.app/Contents/MacOS/codex")
                    .to_string_lossy()
                    .into_owned()
            });
        let app_version = read_bundle_version(&app_path);

        return Ok(Some(RuntimeLocation {
            app_path: app_path.to_string_lossy().into_owned(),
            node: node.to_string_lossy().into_owned(),
            client: client.to_string_lossy().into_owned(),
            codex_home: codex_home.to_string_lossy().into_owned(),
            node_module_dirs: module_dirs.is_dir().then(|| module_dirs.to_string_lossy().into_owned()),
            codex_cli,
            app_version,
        }));
    }

    Ok(None)
}
