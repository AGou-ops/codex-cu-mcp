use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/pip.swift");
    println!("cargo:rerun-if-changed=build.rs");

    if cfg!(target_os = "macos") {
        let out_dir = match std::env::var("OUT_DIR") {
            Ok(v) => v,
            Err(_) => return,
        };

        // Locate target/<profile>/ directory
        let out_path = std::path::PathBuf::from(out_dir);
        let target_dir = match out_path.ancestors().nth(3) {
            Some(p) => p.to_path_buf(),
            None => return,
        };

        let swift_src = std::path::Path::new("assets/pip.swift");
        if !swift_src.exists() {
            return;
        }

        let dest = target_dir.join("codex-cu-pip");
        let _ = Command::new("swiftc")
            .args(["-O", "assets/pip.swift", "-o"])
            .arg(&dest)
            .status();
    }
}
