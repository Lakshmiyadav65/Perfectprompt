use std::path::PathBuf;

fn main() {
    bake_supabase_env();
    tauri_build::build()
}

/// Bake Supabase URL + anon key into the binary at compile time.
///
/// Runtime `dotenvy::dotenv()` only finds the repo's `.env` in dev — the
/// installed `.exe` runs from Program Files with no `.env` alongside it,
/// so `std::env::var("SUPABASE_URL")` returns None and the pipeline
/// silently falls back to the BYOK path. That breaks the daily-quota
/// counter in the sidebar (no `hosted:quota` event is ever emitted).
///
/// Resolution order, falling through on empty:
///   1. `SUPABASE_URL` already set in the build env (CI / explicit).
///   2. `VITE_SUPABASE_URL` from the same env (the Vite bundle already
///      reads this).
///   3. Parse `../.env` ourselves so a plain `cargo build` in the repo
///      Just Works.
///
/// The values are exposed via `env!` at compile time. Both are public
/// (anon key is shipped to the browser bundle already) so baking them
/// in is fine. `cargo:rerun-if-*` directives make rebuilds notice .env
/// edits without forcing a clean.
fn bake_supabase_env() {
    let dotenv = load_repo_dotenv();
    for key in ["SUPABASE_URL", "SUPABASE_ANON_KEY"] {
        let resolved = std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                let vite_key = format!("VITE_{key}");
                std::env::var(&vite_key).ok().filter(|v| !v.is_empty())
            })
            .or_else(|| dotenv.get(key).cloned())
            .or_else(|| dotenv.get(&format!("VITE_{key}")).cloned())
            .unwrap_or_default();
        println!("cargo:rustc-env={key}={resolved}");
        println!("cargo:rerun-if-env-changed={key}");
        println!("cargo:rerun-if-env-changed=VITE_{key}");
    }
}

fn load_repo_dotenv() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    // .env lives at the repo root, one level up from src-tauri/.
    let candidates = [
        manifest_dir.join("../.env"),
        manifest_dir.join(".env"),
    ];
    for path in candidates {
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            if let Ok(contents) = std::fs::read_to_string(&path) {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        let key = k.trim().to_string();
                        // Strip optional surrounding quotes — matches dotenvy.
                        let value = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                        if !key.is_empty() && !value.is_empty() {
                            map.entry(key).or_insert(value);
                        }
                    }
                }
            }
        }
    }
    map
}
