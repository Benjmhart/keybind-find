//! Every plugin template (`templates/`) through its own test and `keybind-find check-plugin`, by
//! `templates/check.sh`, with this build of keybind-find. A template whose interpreter isn't installed is skipped
//! with a message, except under CI, where only Lua may be.

use std::process::Command;

#[test]
fn every_template_passes_its_test_and_check_plugin() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("templates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/check.sh"))
        .arg(env!("CARGO_BIN_EXE_keybind-find"))
        // check-plugin reads the config and the manifest cache: empty ones, not the developer's.
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    // Shown with `--nocapture`, so a skipped template is visible.
    println!("{report}");
    assert!(
        out.status.success(),
        "{report}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
