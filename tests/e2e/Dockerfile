# keybind-find's end-to-end harness: the programs keybind-find reads, as Arch packages them on one date, plus a
# display to run them in. Run through `tests/e2e/run`, not by hand.
#
# Pinned: the base image by its dated tag, and every package by the Arch Linux Archive's snapshot of SNAPSHOT
# (`YYYY/MM/DD`). The same two values give the same versions on every build. To move to newer programs, set SNAPSHOT
# to a later date (and BASE to a base image no newer than it).
ARG BASE=archlinux:base-20261025.0.616225
FROM ${BASE}
ARG SNAPSHOT=2026/10/30

# -Syyuu: refresh against the snapshot and move every package to its version there, up or down.
RUN echo "Server = https://archive.archlinux.org/repos/${SNAPSHOT}/\$repo/os/\$arch" > /etc/pacman.d/mirrorlist \
 && pacman -Syyuu --noconfirm --needed \
      rust gcc python util-linux \
      xorg-server-xvfb xorg-xprop xdotool i3-wm sway dbus mesa vulkan-swrast ttf-dejavu \
      xterm alacritty kitty ghostty foot xfce4-terminal gnome-terminal rxvt-unicode terminator wezterm \
      zsh bash fish tmux screen less \
      helix vim nano micro mc ranger ncmpcpp yazi emacs-nox \
      zathura mpv code obsidian zed xfconf glib2 \
 && pacman -Scc --noconfirm
# sway ships with cap_sys_nice, which a container's capability set doesn't allow: its exec fails with EPERM.
RUN setcap -r /usr/bin/sway

# Programs that refuse to run as root (and a home that looks like a user's). The cache directories are created here
# so the named volumes `run` mounts on them start out owned by this user.
RUN useradd -m -s /bin/bash tester \
 && install -d -o tester -g tester /target /home/tester/.cargo/registry
USER tester
WORKDIR /home/tester
