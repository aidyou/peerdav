fn main() {
    println!("cargo:rerun-if-env-changed=PEERDAV_UPDATER_PUBLIC_KEY");
    tauri_build::build()
}
