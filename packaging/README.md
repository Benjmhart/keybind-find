# Packaging

Recipes for distributions, kept here and submitted to each distribution on release day. Nothing in this directory
is built by `cargo` or CI.

| Recipe | What it is | Where it goes |
|---|---|---|
| `aur/keybind-find/` | Arch `PKGBUILD` + `.SRCINFO`: builds the release's source tarball, runs the tests | AUR package `keybind-find` |
| `aur/keybind-find-bin/` | Arch `PKGBUILD` + `.SRCINFO`: installs the static musl binary from the GitHub release | AUR package `keybind-find-bin` |
| `gentoo/x11-misc/keybind-find/` | ebuild (`cargo.eclass`, `CRATES` from `Cargo.lock` by `pycargoebuild`) + `metadata.xml` | the [GURU](https://wiki.gentoo.org/wiki/Project:GURU) overlay |
| `debian/RFP.txt` | a Request For Package bug, with the Debian versions of the dependency crates | Debian's bug tracker (`wnpp`); Ubuntu's next release syncs it from Debian. Current Ubuntu releases would need a PPA (not done) |

Fedora COPR, openSUSE OBS, Void and others: not done here; contributions welcome.

Checked 2026-10-25 against local tarballs laid out like the release's (the real URLs are first fetched by
`updpkgsums` on release day): both AUR recipes build with `makepkg -s` in a clean `archlinux` container, the source one
running the tests, and pass `namcap` (one warning on `keybind-find`: Rust links `ld-linux` unused). The ebuild builds
and passes its tests in a `gentoo/stage3` container, and `pkgcheck scan` is clean in a scratch overlay with a
generated `Manifest`.

Pre-releases (`vX.Y.Z-rc.N`) are not packaged: the tag's `-` is not valid in `pkgver` or an ebuild version.

## Release day

For version `X.Y.Z`, after the `vX.Y.Z` GitHub release is published:

1. **Version bump.** `pkgver=X.Y.Z` and `pkgrel=1` in both `PKGBUILD`s; `git mv` the ebuild to
   `keybind-find-X.Y.Z.ebuild`; `Version` in `debian/RFP.txt`.
2. **Checksums.** In each AUR directory, `updpkgsums` (replaces the `SKIP` placeholders), then
   `makepkg --printsrcinfo > .SRCINFO`. Gentoo: `pycargoebuild -i keybind-find-X.Y.Z.ebuild <source dir>` to refresh
   `CRATES` from the new `Cargo.lock`; GURU uses thin manifests, so `pkgdev manifest` writes the `Manifest`.
3. **AUR.** For each package, clone `ssh://aur@aur.archlinux.org/<name>.git`, copy in `PKGBUILD` and `.SRCINFO`,
   commit and push (the maintainer's AUR SSH key). Only after step 2: the AUR rejects `SKIP` for a release tarball.
   The `# Maintainer:` line has no email on purpose (as the .deb); add one there if wanted.
4. **GURU.** Add a `<maintainer type="person">` with name and email to `metadata.xml` (GURU requires one), then
   follow the [GURU contributor guide](https://wiki.gentoo.org/wiki/Project:GURU/Information_for_Contributors): once
   given access, push to the `dev` branch of `git@git.gentoo.org:repo/proj/guru.git` (GitHub's `gentoo/guru` is a
   read-only mirror).
5. **Debian.** Re-check the crate versions in `RFP.txt` (`https://api.ftp-master.debian.org/madison?package=rust-x11rb`)
   and send it with `reportbug wnpp` (choose RFP), or mail it to `submit@bugs.debian.org`.
