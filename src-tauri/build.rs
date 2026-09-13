use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::{fs, path::Path};

fn main() {
    let encoded = include_str!("icons/icon-valid.ico.b64").trim();
    let icon = STANDARD
        .decode(encoded)
        .expect("valid embedded Windows icon");
    let icon_path = Path::new("icons/icon.ico");
    fs::write(icon_path, icon).expect("write generated Windows icon");

    tauri_build::build()
}
