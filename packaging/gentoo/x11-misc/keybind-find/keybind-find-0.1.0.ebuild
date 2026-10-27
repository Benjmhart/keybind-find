# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES="
	anyhow@1.0.104
	bitflags@2.13.2
	equivalent@1.0.3
	errno@0.3.14
	gethostname@1.1.0
	hashbrown@0.17.1
	indexmap@2.14.2
	itoa@1.0.18
	libc@0.2.191
	linux-raw-sys@0.12.1
	memchr@2.8.3
	proc-macro2@1.0.107
	quote@1.0.47
	rustix@1.1.5
	serde@1.0.229
	serde_core@1.0.229
	serde_derive@1.0.229
	serde_json@1.0.154
	serde_spanned@0.6.9
	syn@3.0.9
	toml@0.8.23
	toml_datetime@0.6.11
	toml_edit@0.22.27
	toml_write@0.1.2
	unicode-ident@1.0.27
	windows-link@0.2.1
	windows-sys@0.61.2
	winnow@0.7.15
	x11rb-protocol@0.14.0
	x11rb@0.14.0
	zmij@1.0.23
"

RUST_MIN_VER="1.98.0"

inherit cargo optfeature

DESCRIPTION="Search the keybindings for the focused window and run the one you pick"
HOMEPAGE="https://github.com/Benjmhart/keybind-find"
SRC_URI="
	https://github.com/Benjmhart/keybind-find/archive/v${PV}.tar.gz -> ${P}.tar.gz
	${CARGO_CRATE_URIS}
"

LICENSE="MIT"
# Dependent crate licenses
LICENSE+=" Apache-2.0 MIT Unicode-3.0"
SLOT="0"
KEYWORDS="~amd64"
IUSE="test"
RESTRICT="!test? ( test )"

# The nvim provider's tests run a real `nvim --headless`.
BDEPEND="test? ( app-editors/neovim )"

DOCS=( README.md CHANGELOG.md )

src_install() {
	cargo_src_install
	doman man/${PN}.1
	einstalldocs
}

pkg_postinst() {
	optfeature "reading the focused X11 window and sending keys" x11-misc/xdotool
	optfeature "reading the X11 window class, title and pid" x11-apps/xprop
	optfeature "key search and checking config.toml keys" x11-apps/xmodmap
	optfeature "the list on X11 (0.66+ for key search)" app-shells/fzf
	optfeature "the window fzf runs in" x11-terms/alacritty
	optfeature "the list on X11 without fzf or alacritty" x11-misc/dmenu
	# fuzzel and wtype (the list and sending keys on Wayland) and espanso are not in ::gentoo (checked 2026-10-25).
	optfeature "showing errors when run from a hotkey" x11-libs/libnotify
	optfeature "nvim's bindings" app-editors/neovim
	optfeature "tmux's bindings" app-misc/tmux
}
