# xmonad setup

keybind-find doesn't parse your `xmonad.hs`. xmonad reports its own keymap instead. With
[`XMonad.Util.NamedActions`](https://hackage.haskell.org/package/xmonad-contrib/docs/XMonad-Util-NamedActions.html),
every binding carries a name, and `defaultKeysDescr` adds xmonad's stock keys with their own descriptions. Pressing
the help key writes the running keymap to `$XDG_CACHE_HOME/keybind-find/xmonad.keys` (default
`~/.cache/keybind-find/xmonad.keys`) and opens keybind-find. The file is rewritten on every press, so the list you
see from the key matches the xmonad you're running. Running `keybind-find` some other way reads whatever was written
last.

## Requirements

- xmonad-contrib **0.16 or newer**. keybind-find reads both key formats NamedActions writes: 0.16's
  (`M4-Shift-Return`) and the one from 0.17.1 on (`M4-S-<Return>`, `M4--` for Super+minus).
- `keybind-find` on the `PATH` xmonad was started with (xmonad's `spawn` uses that, not your shell's). If it lives in
  `~/.cargo/bin` and your session doesn't have that on `PATH`, write the full path in `spawn`.
- `ewmh` from `XMonad.Hooks.EwmhDesktops` (in the setup below), so keybind-find can see the focused window
  (`xdotool getactivewindow`). Without it, only xmonad's and espanso's keys are listed.

## Recommended key: `Super+/`

`Super+/` is free in stock xmonad. If you want a different key, change only the `(mod4Mask, xK_slash)` tuple below.

## Setup

```haskell
import XMonad
import XMonad.Util.NamedActions
import XMonad.Hooks.EwmhDesktops (ewmh)
import System.Directory (createDirectoryIfMissing, getXdgDirectory, XdgDirectory(XdgCache))

main = xmonad $ ewmh $ addDescrKeys ((mod4Mask, xK_slash), keybindFind) myKeys
              $ def { modMask = mod4Mask }

myKeys _ =
  [ subtitle "custom"
  , ((mod4Mask, xK_b), addName "Open a browser" $ spawn "firefox")
  ]

-- Super+/: write the running keymap, then open keybind-find.
keybindFind :: [((KeyMask, KeySym), NamedAction)] -> NamedAction
keybindFind km = addName "Search keybindings (keybind-find)" $ do
  io $ do
    dir <- getXdgDirectory XdgCache "keybind-find"
    createDirectoryIfMissing True dir
    writeFile (dir ++ "/xmonad.keys") (unlines (showKmSimple km))
  spawn "keybind-find"
```

Then `xmonad --recompile && xmonad --restart`, press `Super+/`, and check that
`~/.cache/keybind-find/xmonad.keys` exists.

## Converting an existing config

- Wrap each action in `addName "<description>" $ …`, for example
  `((mod4Mask, xK_b), addName "Open a browser" $ spawn "firefox")`.
- Replace ``def { … } `additionalKeys` myKeys`` with `addDescrKeys ((mod4Mask, xK_slash), keybindFind) (const myKeys)
  $ def { … }`, or make `myKeys` a function of the config as above.
- Start your list with a `subtitle` (e.g. `subtitle "custom"`). The heading becomes the category keybind-find shows
  your keys under. Without one, they are filed under whatever stock section comes last.
- Your keys still override stock keys on the same chord, exactly as `additionalKeys` did.
- Only keys in your list and in `defaultKeysDescr` are listed. Keys added another way (`additionalKeys` applied after
  `addDescrKeys`, or the `keys` field of the config) still work but don't appear. On 0.16 `addDescrKeys` replaces the
  config's `keys`, so stock `Super+Shift+/` (xmonad's own help) goes away; from 0.18 it keeps working but isn't
  listed.

## Build note

If you build xmonad with stack or cabal (an `xmonad-build.cabal` or `package.yaml` next to `xmonad.hs`), add
`directory` to its dependencies for `System.Directory`.

## Caveat

Keyboards that send Super while a key is held (home-row or space-as-Super layouts) can fire `Super+/` by accident, for
example when a held Space is followed by `/`.
