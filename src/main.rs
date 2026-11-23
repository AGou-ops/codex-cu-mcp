mod disclaimed_spawn;
mod pip;
mod runtime;
mod tools;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const MAX_FRAME: usize = 8 * 1024 * 1024;
const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

enum Event {
    Client(Vec<u8>),
    Child(Vec<u8>),
    ClientClosed,
    ChildClosed,
}

#[derive(Debug)]
struct PendingApproval {
    child_id: Value,
    started: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalMode {
    Client,
    Auto,
}

impl ApprovalMode {
    fn current() -> Self {
        match std::env::var("CODEX_CU_APPROVAL").ok().as_deref() {
            Some("auto") => Self::Auto,
            _ => Self::Client,
        }
    }
}

struct Forwarder {
    child_stdin: Box<dyn Write + Send>,
    pending: HashMap<String, PendingApproval>,
    next_token: u64,
    approval: ApprovalMode,
    client_supports_elicitation: Option<bool>,
    timeout: Duration,
    tools_init_id: Option<String>,
    pip: pip::PipManager,
}
impl Forwarder {
    fn run(&mut self, receiver: Receiver<Event>) -> Result<()> {
        loop {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(Event::Client(frame)) => self.on_client(&frame)?,
                Ok(Event::Child(frame)) => self.on_child(&frame)?,
                Ok(Event::ClientClosed) | Ok(Event::ChildClosed) => {
                    self.cancel_pending()?;
                    return Ok(());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => self.expire_pending()?,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.cancel_pending()?;
                    return Ok(());
                }
            }
            self.expire_pending()?;
        }
    }

    fn on_client(&mut self, frame: &[u8]) -> Result<()> {
        if let Ok(mut message) = serde_json::from_slice::<Value>(frame) {
            if message.get("method").and_then(Value::as_str) == Some("initialize") {
                self.client_supports_elicitation = Some(supports_form_elicitation(&message));
                declare_elicitation(&mut message);
                self.tools_init_id = message.get("id").map(Value::to_string);
                return self.write_child(&message);
            }
            if message.get("method").and_then(Value::as_str) == Some("tools/call") {
                if let Some(reply) = tools::reject_untypeable(&message) {
                    return write_client(&reply);
                }
            }
            if message.get("method").is_none()
                && (message.get("result").is_some() || message.get("error").is_some())
                && let Some(token) = message.get("id").and_then(Value::as_str)
                && let Some(pending) = self.pending.remove(token)
            {
                return self.relay_approval(pending, &message);
            }
        }
        self.write_child_bytes(frame)
    }

    fn on_child(&mut self, frame: &[u8]) -> Result<()> {
        if let Some((app_name, bundle_id, pid)) = pip::parse_app_activity(frame) {
            self.pip.send_target(&app_name, &bundle_id, pid);
        }
        if let Some(image_data) = pip::extract_frame_image(frame) {
            self.pip.send_frame(&image_data);
        }

        if frame
            .windows(b"\"elicitation/create\"".len())
            .any(|window| window == b"\"elicitation/create\"")
        {
            if let Ok(message) = serde_json::from_slice::<Value>(frame) {
                if message.get("method").and_then(Value::as_str) == Some("elicitation/create") {
                    return self.on_elicitation(message);
                }
            }
        }

        if let Some(init_id) = &self.tools_init_id {
            if !frame.windows(b"\"method\"".len()).any(|w| w == b"\"method\"") {
                if let Ok(mut message) = serde_json::from_slice::<Value>(frame) {
                    if message.get("id").map(Value::to_string).as_deref() == Some(init_id) {
                        self.tools_init_id = None;
                        tools::add_instructions(&mut message);
                        return write_client(&message);
                    }
                }
            }
        }

        write_client_bytes(frame)
    }

    fn on_elicitation(&mut self, message: Value) -> Result<()> {
        let child_id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        if self.approval == ApprovalMode::Auto || self.client_supports_elicitation == Some(false) {
            return self.reply_to_child(&child_id, json!({ "action": "accept", "content": {} }));
        }
        self.next_token += 1;
        let token = format!("codex-cu-elicit-{}", self.next_token);
        self.pending.insert(
            token.clone(),
            PendingApproval {
                child_id,
                started: Instant::now(),
            },
        );
        write_client(&json!({
            "jsonrpc": "2.0",
            "id": token,
            "method": "elicitation/create",
            "params": params,
        }))
    }

    fn relay_approval(&mut self, pending: PendingApproval, message: &Value) -> Result<()> {
        let result = message
            .get("result")
            .cloned()
            .unwrap_or_else(|| json!({ "action": "cancel", "content": {} }));
        self.reply_to_child(&pending.child_id, result)
    }

    fn reply_to_child(&mut self, id: &Value, result: Value) -> Result<()> {
        self.write_child(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn expire_pending(&mut self) -> Result<()> {
        let expired = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.started.elapsed() >= self.timeout)
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        for token in expired {
            if let Some(pending) = self.pending.remove(&token) {
                self.reply_to_child(
                    &pending.child_id,
                    json!({ "action": "cancel", "content": {} }),
                )?;
            }
        }
        Ok(())
    }

    fn cancel_pending(&mut self) -> Result<()> {
        let pending = self
            .pending
            .drain()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        for item in pending {
            self.reply_to_child(&item.child_id, json!({ "action": "cancel", "content": {} }))?;
        }
        Ok(())
    }

    fn write_child(&mut self, message: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        self.write_child_bytes(&bytes)
    }

    fn write_child_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.child_stdin.write_all(bytes)?;
        self.child_stdin.flush()?;
        Ok(())
    }
}

fn spawn_reader<R: Read + Send + 'static>(reader: R, sender: Sender<Event>, client: bool) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut frame = Vec::new();
            match reader.read_until(b'\n', &mut frame) {
                Ok(0) => break,
                Ok(size) if size <= MAX_FRAME => {
                    let event = if client {
                        Event::Client(frame)
                    } else {
                        Event::Child(frame)
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
                Ok(_) => {
                    // oversized frame
                }
                Err(_) => break,
            }
        }
        let _ = sender.send(if client {
            Event::ClientClosed
        } else {
            Event::ChildClosed
        });
    });
}

fn serve() -> Result<()> {
    let runtime = runtime::locate_runtime()?
        .context("未找到 Codex 运行时（ChatGPT.app），Computer Use 不可用")?;
    let shim = runtime::ensure_shim()?;

    let mut envs: Vec<(String, String)> = runtime.native_client_env().into_iter().collect();
    envs.push(("CODEX_PLUS_CU_CLIENT".into(), runtime.client.clone()));
    envs.push(("CODEX_CU_CLIENT".into(), runtime.client.clone()));

    let mut child = disclaimed_spawn::spawn_clean(
        Path::new(&runtime.node),
        &[OsStr::new(&shim)],
        &envs,
    )?;

    let child_stdin = child.stdin.take().context("CU 子进程标准输入不可用")?;
    let child_stdout = child.stdout.take().context("CU 子进程标准输出不可用")?;

    let (sender, receiver) = mpsc::channel();
    spawn_reader(std::io::stdin(), sender.clone(), true);
    spawn_reader(child_stdout, sender, false);

    let timeout = std::env::var("CODEX_CU_APPROVAL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_APPROVAL_TIMEOUT);

    let mut forwarder = Forwarder {
        child_stdin,
        pending: HashMap::new(),
        next_token: 0,
        approval: ApprovalMode::current(),
        client_supports_elicitation: None,
        timeout,
        tools_init_id: None,
        pip: pip::PipManager::new(),
    };

    let result = forwarder.run(receiver);
    child.kill();
    child.wait();
    result
}

fn supports_form_elicitation(message: &Value) -> bool {
    match message.pointer("/params/capabilities/elicitation") {
        Some(Value::Object(modes)) => {
            modes.is_empty() || modes.get("form").is_some_and(|form| !form.is_null())
        }
        _ => false,
    }
}

fn declare_elicitation(message: &mut Value) {
    let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) else {
        return;
    };
    let capabilities = params
        .entry("capabilities")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !capabilities.is_object() {
        *capabilities = Value::Object(serde_json::Map::new());
    }
    if let Some(capabilities) = capabilities.as_object_mut() {
        capabilities
            .entry("elicitation")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
}

fn write_client(message: &Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, message)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn write_client_bytes(bytes: &[u8]) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("codex-cu {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--check-runtime") => {
            let runtime = runtime::locate_runtime()?
                .context("未找到 Codex 运行时（ChatGPT.app），Computer Use 不可用")?;
            println!(
                "runtime=ok node={} client={} codex_cli={}",
                runtime.node,
                runtime.client,
                runtime
                    .codex_cli
                    .as_deref()
                    .unwrap_or("missing")
            );
            Ok(())
        }
        Some(argument) => bail!("未知参数：{argument}"),
        None => serve(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_capability_is_added_without_losing_client_info() {
        let mut message = json!({
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "test" },
                "capabilities": { "roots": { "listChanged": true } }
            }
        });
        assert!(!supports_form_elicitation(&message));
        declare_elicitation(&mut message);
        assert_eq!(message["params"]["clientInfo"]["name"], "test");
        assert!(message["params"]["capabilities"]["elicitation"].is_object());
    }

    #[test]
    fn declared_form_elicitation_is_detected() {
        let message = json!({
            "params": { "capabilities": { "elicitation": { "form": {}, "url": {} } } }
        });
        assert!(supports_form_elicitation(&message));
    }
}
