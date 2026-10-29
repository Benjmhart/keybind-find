//! Provider registry: one module per program, picked by name.

use crate::context::Context;
use crate::sheet::Sheet;

mod alacritty;
mod alacritty_defaults;
mod espanso;
mod ghostty;
mod herdr;
mod hyprland;
mod kde;
pub(crate) mod kitty;
mod less;
mod mpv;
mod nano;
mod nvim;
mod pinned;
// `src/plugins.rs`, declared here because `main.rs` is not this story's to edit.
#[path = "../plugins.rs"]
pub mod plugins;
mod screen;
mod sheets;
mod shells;
mod sway;
mod tmux;
mod vim;
mod vimium;
mod xmonad;

pub(crate) use sheets::is_browser;

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
];

pub fn sheet(name: &str, ctx: &Context) -> anyhow::Result<Sheet> {
    match name {
        "xmonad" => xmonad::sheet(ctx),
        "sway" => sway::sheet(ctx),
        "hyprland" => hyprland::sheet(ctx),
        "kde" => kde::sheet(ctx),
        "espanso" => espanso::sheet(ctx),
        "nvim" => nvim::sheet(ctx),
        "herdr" => herdr::sheet(ctx),
        "alacritty" => alacritty::sheet(ctx),
        "ghostty" => ghostty::sheet(ctx),
        "sheets" => sheets::sheet(ctx),
        "tmux" => tmux::sheet(ctx),
        "vimium" => vimium::sheet(ctx),
        "bash" | "zsh" | "fish" => shells::sheet(name),
        "less" => less::sheet(),
        "mpv" => mpv::sheet(),
        "vim" => vim::sheet(ctx),
        "nano" => nano::sheet(),
        "screen" => screen::sheet(),
        "kitty" => kitty::sheet(),
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
        assert_eq!(sheet("alacritty", &ctx).unwrap().provider, "alacritty");
        assert_eq!(sheet("sheets", &ctx).unwrap().provider, "sheets");
        // kitty falls back to its pinned defaults with no kitty installed.
        assert_eq!(sheet("kitty", &ctx).unwrap().provider, "kitty");
        for p in KNOWN {
            if let Err(e) = sheet(p, &ctx) {
                assert!(!e.to_string().contains("unknown provider"), "{p}: {e}");
            }
        }
    }
}
