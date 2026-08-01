use std::{env, fs, path::PathBuf};

fn emit_env_from_dotenv(key: &str) {
    if let Ok(value) = env::var(key) {
        println!("cargo:rerun-if-env-changed={key}");
        if !value.trim().is_empty() && !value.contains(['\r', '\n']) {
            println!("cargo:rustc-env={key}={value}");
        }
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let env_path = manifest_dir.join(".env");
    println!("cargo:rerun-if-changed={}", env_path.display());

    let Ok(contents) = fs::read_to_string(env_path) else {
        return;
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };

        if name.trim() == key {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.contains(['\r', '\n']) {
                println!("cargo:rustc-env={key}={value}");
            }
            return;
        }
    }
}

fn main() {
    emit_env_from_dotenv("GOOGLE_CLIENT_ID");
    emit_env_from_dotenv("GOOGLE_CLIENT_SECRET");
    emit_env_from_dotenv("MICROSOFT_CLIENT_ID");
    tauri_build::build()
}
