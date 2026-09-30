// The web UI is embedded from ui/build. Create a placeholder so the server builds
// even when the UI has not been built (`pnpm -C ui build`).
fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/build");
    if !dir.join("index.html").exists() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>Sparkles</title><p>The UI has not been built. Run <code>pnpm -C ui install &amp;&amp; pnpm -C ui build</code> and rebuild the server.</p>",
        )
        .unwrap();
    }
    println!("cargo:rerun-if-changed=../../ui/build");
}
