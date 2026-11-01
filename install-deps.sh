#!/bin/sh
# Installs the programs keybind-find runs, with your distribution's package manager (README.md, "Requirements"):
#   curl -fsSL https://raw.githubusercontent.com/Benjmhart/keybind-find/main/install-deps.sh | sh
# Supports Arch, Debian/Ubuntu, Fedora and openSUSE (and their derivatives, via /etc/os-release's ID_LIKE).
# It installs keybind-find's dependencies, not keybind-find itself, and not the apps it reads (nvim, tmux, sway...).
set -eu

[ -r /etc/os-release ] || { echo "install-deps.sh: no /etc/os-release; install the README's Requirements by hand" >&2; exit 1; }
# shellcheck disable=SC1091
. /etc/os-release

# The X11 set: the focused window, sending keys, the fzf list in alacritty, key search, error notifications.
# On Wayland (not GNOME, which keeps fzf: no layer-shell for fuzzel) add fuzzel and wtype.
pm=
for id in $ID ${ID_LIKE:-}; do
  case $id in
    arch)
      pm="pacman -S --needed --noconfirm"
      x11="xdotool xorg-xprop xorg-xmodmap fzf alacritty libnotify" ;;
    debian | ubuntu)
      pm="apt-get install -y"
      x11="xdotool x11-utils x11-xserver-utils fzf alacritty libnotify-bin" ;;
    fedora)
      pm="dnf install -y"
      x11="xdotool xprop xmodmap fzf alacritty libnotify" ;;
    opensuse* | suse)
      pm="zypper --non-interactive install"
      x11="xdotool xprop xmodmap fzf alacritty libnotify-tools" ;;
    *) continue ;;
  esac
  break
done
[ -n "$pm" ] || { echo "install-deps.sh: $ID isn't supported; install the README's Requirements by hand" >&2; exit 1; }

pkgs=$x11
case ${XDG_SESSION_TYPE:-}${WAYLAND_DISPLAY:+wayland} in
  *wayland*) case ${XDG_CURRENT_DESKTOP:-} in *GNOME*) ;; *) pkgs="$pkgs fuzzel wtype" ;; esac ;;
esac

sudo=; [ "$(id -u)" -eq 0 ] || sudo=sudo
echo "Installing: $pkgs"
# A fresh container has no package lists yet.
case $pm in apt-get*) $sudo apt-get update ;; esac
# shellcheck disable=SC2086 # word splitting is the point
$sudo $pm $pkgs

# fzf 0.60 runs the list, 0.66 adds key search; some stable releases ship older.
v=$(fzf --version | cut -d' ' -f1)
case $(printf '%s\n0.66\n' "$v" | sort -V | head -1) in
  0.66) ;;
  *) echo "Note: fzf $v is older than 0.66, so no key search. A newer fzf: https://github.com/junegunn/fzf/releases"
    case $(printf '%s\n0.60\n' "$v" | sort -V | head -1) in
      0.60) ;;
      *) echo "Below 0.60 the fzf list doesn't run: install dmenu and set [picker] kind = \"dmenu\" (docs/picker.md)." ;;
    esac ;;
esac
echo "Done. Now install keybind-find itself: see README.md, Install."
