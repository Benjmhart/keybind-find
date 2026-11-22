//! Provider registry: one module per program, picked by name.

use crate::context::Context;
use crate::sheet::{Sheet, Source};

mod alacritty;
mod alacritty_defaults;
mod awesome;
mod emacs;
mod espanso;
mod foot;
mod ghostty;
mod gimp;
mod gnome;
mod gnome_terminal;
mod helix;
mod herdr;
mod hyprland;
mod i3;
mod inkscape;
mod kakoune;
mod kde;
mod kdl_v1;
pub(crate) mod kitty;
mod konsole;
mod lazygit;
mod less;
mod lf;
mod libreoffice;
mod mc;
mod micro;
mod mpv;
mod nano;
mod ncmpcpp;
mod niri;
mod nnn;
mod nvim;
mod obsidian;
mod openbox;
mod pinned;
mod qutebrowser;
mod ranger;
// `src/plugins.rs`, declared here because `main.rs` is not this story's to edit.
#[path = "../plugins.rs"]
pub mod plugins;
mod screen;
mod sheets;
mod shells;
mod sway;
mod sxhkd;
mod tmux;
mod vim;
mod vimium;
mod vscode;
mod wezterm;
mod xfce;
mod xfce4_terminal;
mod xmonad;
mod yaml_subset;
mod yazi;
mod zathura;
mod zed;
mod zellij;

pub(crate) use gimp::is_gimp;
pub(crate) use inkscape::is_inkscape;
pub(crate) use libreoffice::is_libreoffice;
pub(crate) use qutebrowser::is_qutebrowser;
pub(crate) use sheets::is_browser;
pub(crate) use sxhkd::running as sxhkd_running;
pub(crate) use vscode::is_vscode;
pub(crate) use zed::is_zed;

pub const KNOWN: &[&str] = &[
    "xmonad",
    "espanso",
    "nvim",
    "herdr",
    "sway",
    "alacritty",
    "sheets",
    "tmux",
    "vimium",
    "bash",
    "zsh",
    "fish",
    "less",
    "mpv",
    "vim",
    "nano",
    "hyprland",
    "kde",
    "ghostty",
    "screen",
    "kitty",
    "xfce",
    "i3",
    "yazi",
    "zathura",
    "micro",
    "gnome",
    "foot",
    "obsidian",
    "gnome-terminal",
    "mc",
    "vscode",
    "emacs",
    "zed",
    "ncmpcpp",
    "ranger",
    "xfce4_terminal",
    "helix",
    "wezterm",
    "zellij",
    "sxhkd",
    "konsole",
    "awesome",
    "niri",
    "lf",
    "lazygit",
    "nnn",
    "kakoune",
    "openbox",
    "qutebrowser",
    "gimp",
    "inkscape",
    "libreoffice",
];

/// The versions of the built-in defaults of zed (`data/zed/<v>/`) and alacritty (`alacritty_defaults.rs`), which have
/// no version probe.
const ZED_DEFAULTS: &str = "1.25.3";
const ALACRITTY_DEFAULTS: &str = "0.17.0";

/// The sheet of provider `name`, and where its keys came from when they include built-in defaults.
pub fn sheet(name: &str, ctx: &Context) -> anyhow::Result<(Sheet, Option<Source>)> {
    match name {
        "foot" => foot::sheet(),
        "gimp" => gimp::sheet(),
        "helix" => helix::sheet(),
        "inkscape" => inkscape::sheet(),
        "herdr" => herdr::sheet(ctx),
        "kakoune" => kakoune::sheet(),
        "kitty" => kitty::sheet(),
        "konsole" => konsole::sheet(),
        "lazygit" => lazygit::sheet(),
        "lf" => lf::sheet(),
        // LibreOffice's defaults come from its own install, not from keybind-find: no `Source` (10.1).
        "libreoffice" => libreoffice::sheet(ctx).map(|s| (s, None)),
        "mc" => mc::sheet(),
        "micro" => micro::sheet(),
        "nano" => nano::sheet(),
        "ncmpcpp" => ncmpcpp::sheet(),
        "nnn" => nnn::sheet(),
        "obsidian" => obsidian::sheet(ctx),
        "qutebrowser" => qutebrowser::sheet(ctx),
        "ranger" => ranger::sheet(),
        "screen" => screen::sheet(),
        "vimium" => vimium::sheet(ctx),
        "vscode" => vscode::sheet(ctx),
        "xfce4_terminal" => xfce4_terminal::sheet(),
        "yazi" => yazi::sheet(),
        "zathura" => zathura::sheet(ctx),
        "zed" => zed::sheet(ctx).map(|s| (s, Some(Source::unchecked(ZED_DEFAULTS)))),
        "alacritty" => {
            alacritty::sheet(ctx).map(|s| (s, Some(Source::unchecked(ALACRITTY_DEFAULTS))))
        }
        _ => other(name, ctx).map(|s| (s, None)),
    }
}

/// The text of the file at `path`, read whole: `None` when it doesn't exist; else why it can't be read (not a regular
/// file, larger than `max` bytes, not UTF-8, an I/O error).
pub(super) fn read_capped(path: &std::path::Path, max: u64) -> Result<Option<String>, String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt;
    // Opened non-blocking and checked on the handle, so a FIFO put there can't hang us.
    // ponytail: Linux's O_NONBLOCK on x86 and arm (libc isn't a dependency); alpha, mips, parisc and sparc differ.
    const O_NONBLOCK: i32 = 0o4000;
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(f) => f,
    };
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("larger than {} MiB", max >> 20));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "not UTF-8".to_string())
}

/// The providers whose keys never come from built-in defaults, and plugins.
fn other(name: &str, ctx: &Context) -> anyhow::Result<Sheet> {
    match name {
        "xmonad" => xmonad::sheet(ctx),
        "sway" => sway::sheet(ctx),
        "hyprland" => hyprland::sheet(ctx),
        "kde" => kde::sheet(ctx),
        "xfce" => xfce::sheet(),
        "gnome" => gnome::sheet(),
        "gnome-terminal" => gnome_terminal::sheet(),
        "i3" => i3::sheet(ctx),
        "espanso" => espanso::sheet(ctx),
        "nvim" => nvim::sheet(ctx),
        "ghostty" => ghostty::sheet(ctx),
        "sheets" => sheets::sheet(ctx),
        "tmux" => tmux::sheet(ctx),
        "bash" | "zsh" | "fish" => shells::sheet(name),
        "less" => less::sheet(),
        "mpv" => mpv::sheet(),
        "vim" => vim::sheet(ctx),
        "emacs" => emacs::sheet(ctx),
        "wezterm" => wezterm::sheet(ctx),
        "zellij" => zellij::sheet(),
        "sxhkd" => sxhkd::sheet(ctx),
        "awesome" => awesome::sheet(ctx),
        "niri" => niri::sheet(ctx),
        "openbox" => openbox::sheet(ctx),
        _ if let Some(sheet) = plugins::sheet(name, ctx) => sheet,
        _ => anyhow::bail!(
            "unknown provider {name:?}; known providers: {}",
            KNOWN.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_provider_lists_known() {
        let ctx = Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: vec![],
        };
        let err = sheet("nosuch", &ctx).unwrap_err().to_string();
        for p in KNOWN {
            assert!(err.contains(p), "{err}");
        }
    }

    #[test]
    fn unchecked_pins_match_their_data() {
        let zed = format!("{}/data/zed/{ZED_DEFAULTS}", env!("CARGO_MANIFEST_DIR"));
        assert!(std::path::Path::new(&zed).is_dir(), "{zed}");
        let alacritty = include_str!("alacritty_defaults.rs");
        assert!(
            alacritty
                .lines()
                .next()
                .is_some_and(|l| l.contains(&format!("alacritty {ALACRITTY_DEFAULTS}"))),
            "alacritty_defaults.rs names another version"
        );
    }

    #[test]
    fn every_known_name_has_an_arm() {
        let ctx = Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: vec![],
        };
        assert!(KNOWN.contains(&"alacritty") && KNOWN.contains(&"sway"));
        // The arm reaches the alacritty provider (it falls back to defaults, so this holds on any host).
        let (alacritty, source) = sheet("alacritty", &ctx).unwrap();
        assert_eq!(alacritty.provider, "alacritty");
        assert_eq!(source, Some(Source::unchecked("0.17.0")));
        assert_eq!(
            sheet("zed", &ctx).ok().map(|(_, s)| s),
            Some(Some(Source::unchecked("1.25.3")))
        );
        assert_eq!(sheet("sheets", &ctx).unwrap().0.provider, "sheets");
        // kitty falls back to its pinned defaults with no kitty installed.
        assert_eq!(sheet("kitty", &ctx).unwrap().0.provider, "kitty");
        for p in KNOWN {
            if let Err(e) = sheet(p, &ctx) {
                assert!(!e.to_string().contains("unknown provider"), "{p}: {e}");
            }
        }
    }
}
