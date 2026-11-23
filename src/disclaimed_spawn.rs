//! 以「放弃责任归属」的方式拉起子进程（macOS）。
//!
//! # 为什么需要
//!
//! macOS 给每个进程记一个 responsible process（TCC 授权与代码签名校验都按它算）。
//! 默认情况下子进程继承父进程的责任进程：从终端起，责任进程是终端；从某些调用方起，
//! 责任进程可能没有正确的 OpenAI Team ID。
//!
//! Codex 的原生 CU 客户端会校验这条链上的责任进程签名（`_senderResponsibleTeamID`
//! 等字段）。通过让 OpenAI 签名的 `cua_node/bin/node` 成为自己的责任进程，所有调用方都能正常通过。
//! 这里用的 `responsibility_spawnattrs_setdisclaim` 是 libSystem 导出的符号。

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::Path;

/// 被拉起的子进程：标准输入输出各一条管道，标准错误继承父进程。
pub struct DisclaimedChild {
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    inner: Inner,
}

enum Inner {
    #[cfg(target_os = "macos")]
    Spawned { pid: i32, reaped: bool },
    #[allow(dead_code)]
    Command(std::process::Child),
}

impl DisclaimedChild {
    #[allow(dead_code)]
    pub fn id(&self) -> u32 {
        match &self.inner {
            #[cfg(target_os = "macos")]
            Inner::Spawned { pid, .. } => *pid as u32,
            Inner::Command(child) => child.id(),
        }
    }

    pub fn kill(&mut self) {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            Inner::Spawned { pid, reaped } => {
                if !*reaped {
                    unsafe { macos::kill(*pid, macos::SIGKILL) };
                }
            }
            Inner::Command(child) => {
                let _ = child.kill();
            }
        }
    }

    pub fn wait(&mut self) {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            Inner::Spawned { pid, reaped } => {
                if !*reaped {
                    let mut status = 0;
                    unsafe { macos::waitpid(*pid, &mut status, 0) };
                    *reaped = true;
                }
            }
            Inner::Command(child) => {
                let _ = child.wait();
            }
        }
    }
}

impl Drop for DisclaimedChild {
    fn drop(&mut self) {
        self.kill();
        self.wait();
    }
}

/// 拉起 `program args…`，环境**只有** `envs`，不继承当前进程的任何变量。
pub fn spawn_clean(
    program: &Path,
    args: &[&OsStr],
    envs: &[(String, String)],
) -> anyhow::Result<DisclaimedChild> {
    let env = envs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    spawn_with_env(program, args, env)
}

fn spawn_with_env(
    program: &Path,
    args: &[&OsStr],
    env: BTreeMap<OsString, OsString>,
) -> anyhow::Result<DisclaimedChild> {
    #[cfg(target_os = "macos")]
    {
        macos::spawn(program, args, &env)
    }
    #[cfg(not(target_os = "macos"))]
    {
        use anyhow::Context;
        use std::process::{Command, Stdio};
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .with_context(|| format!("无法启动 {}", program.display()))?;
        let stdin = child.stdin.take().map(|pipe| Box::new(pipe) as Box<dyn Write + Send>);
        let stdout = child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
        Ok(DisclaimedChild {
            stdin,
            stdout,
            inner: Inner::Command(child),
        })
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeMap;
    use std::ffi::{CString, OsStr, OsString, c_char, c_int, c_short, c_void};
    use std::os::fd::AsRawFd;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use anyhow::{Context, bail};

    use super::{DisclaimedChild, Inner};

    type PosixSpawnAttr = *mut c_void;
    type PosixSpawnFileActions = *mut c_void;

    pub(super) const SIGKILL: c_int = 9;
    const STDIN: c_int = 0;
    const STDOUT: c_int = 1;

    unsafe extern "C" {
        fn posix_spawnattr_init(attr: *mut PosixSpawnAttr) -> c_int;
        fn posix_spawnattr_destroy(attr: *mut PosixSpawnAttr) -> c_int;
        fn posix_spawnattr_setflags(attr: *mut PosixSpawnAttr, flags: c_short) -> c_int;
        fn posix_spawn_file_actions_init(actions: *mut PosixSpawnFileActions) -> c_int;
        fn posix_spawn_file_actions_destroy(actions: *mut PosixSpawnFileActions) -> c_int;
        fn posix_spawn_file_actions_adddup2(
            actions: *mut PosixSpawnFileActions,
            fd: c_int,
            new_fd: c_int,
        ) -> c_int;
        fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            actions: *const PosixSpawnFileActions,
            attr: *const PosixSpawnAttr,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> c_int;
        fn responsibility_spawnattrs_setdisclaim(attr: *mut PosixSpawnAttr, disclaim: c_int) -> c_int;
        pub(super) fn waitpid(pid: i32, status: *mut c_int, options: c_int) -> i32;
        pub(super) fn kill(pid: i32, signal: c_int) -> c_int;
    }

    const POSIX_SPAWN_SETPGROUP: c_short = 0x0002;

    fn cstring(value: &OsStr) -> anyhow::Result<CString> {
        CString::new(value.as_bytes()).context("参数里含有 NUL 字节")
    }

    pub(super) fn spawn(
        program: &Path,
        args: &[&OsStr],
        env: &BTreeMap<OsString, OsString>,
    ) -> anyhow::Result<DisclaimedChild> {
        let (stdin_read, stdin_write) = std::io::pipe().context("无法创建标准输入管道")?;
        let (stdout_read, stdout_write) = std::io::pipe().context("无法创建标准输出管道")?;

        let path = cstring(program.as_os_str())?;
        let mut argv_owned = vec![path.clone()];
        for arg in args {
            argv_owned.push(cstring(arg)?);
        }
        let mut argv: Vec<*const c_char> = argv_owned.iter().map(|value| value.as_ptr()).collect();
        argv.push(std::ptr::null());

        let mut env_owned = Vec::with_capacity(env.len());
        for (key, value) in env {
            let mut pair = key.as_bytes().to_vec();
            pair.push(b'=');
            pair.extend_from_slice(value.as_bytes());
            if let Ok(pair) = CString::new(pair) {
                env_owned.push(pair);
            }
        }
        let mut envp: Vec<*const c_char> = env_owned.iter().map(|value| value.as_ptr()).collect();
        envp.push(std::ptr::null());

        let mut attr: PosixSpawnAttr = std::ptr::null_mut();
        let mut actions: PosixSpawnFileActions = std::ptr::null_mut();
        let mut pid: i32 = 0;
        let code = unsafe {
            if posix_spawnattr_init(&mut attr) != 0 {
                bail!("posix_spawnattr_init 失败");
            }
            if posix_spawn_file_actions_init(&mut actions) != 0 {
                posix_spawnattr_destroy(&mut attr);
                bail!("posix_spawn_file_actions_init 失败");
            }
            let mut code = posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETPGROUP);
            if code == 0 {
                code = responsibility_spawnattrs_setdisclaim(&mut attr, 1);
            }
            if code == 0 {
                code = posix_spawn_file_actions_adddup2(&mut actions, stdin_read.as_raw_fd(), STDIN);
            }
            if code == 0 {
                code = posix_spawn_file_actions_adddup2(&mut actions, stdout_write.as_raw_fd(), STDOUT);
            }
            if code == 0 {
                code = posix_spawn(
                    &mut pid,
                    path.as_ptr(),
                    &actions,
                    &attr,
                    argv.as_ptr(),
                    envp.as_ptr(),
                );
            }
            posix_spawn_file_actions_destroy(&mut actions);
            posix_spawnattr_destroy(&mut attr);
            code
        };
        if code != 0 {
            bail!("posix_spawn 失败：errno={code}");
        }

        let stdin = Box::new(stdin_write) as Box<dyn Write + Send>;
        let stdout = Box::new(stdout_read) as Box<dyn Read + Send>;
        Ok(DisclaimedChild {
            stdin: Some(stdin),
            stdout: Some(stdout),
            inner: Inner::Spawned { pid, reaped: false },
        })
    }
}
