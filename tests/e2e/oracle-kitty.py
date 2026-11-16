# kitty's own effective keymap, for tests/e2e/oracle.py: `kitty +runpy "$(cat oracle-kitty.py)" KITTY.CONF`.
# Loads the config with kitty's own reader (load_config: defaults, kitty_mod, clear_all_shortcuts, includes, modes)
# and prints one line per shortcut: MODE<TAB>KEYS<TAB>FLAGS, KEYS as kitty names them (`ctrl+shift+page_up`, a
# sequence joined by `>`), FLAGS `focus` for a --when-focus-on map. Enumerated as kitty's debug_config.py
# build_shortcut_map does. kitty internals, pinned by the image's kitty (0.49.2).
# One function: +runpy exec()s this inside a function, so top-level names aren't visible to nested scopes.


def main():
    import sys
    from kitty.config import load_config
    from kitty.fast_data_types import glfw_get_key_name
    from kitty.types import mod_to_names

    def name(k):
        key = (glfw_get_key_name(0, k.key) if k.is_native else glfw_get_key_name(k.key, 0)) or f'0x{k.key:x}'
        return '+'.join([*mod_to_names(k.mods), {' ': 'space'}.get(key, key)])

    opts = load_config(sys.argv[-1])
    for mode, km in opts.keyboard_modes.items():
        for defns in km.keymap.values():
            by_seq = {}
            for d in defns:
                by_seq.setdefault(d.full_key_sequence_to_trigger, []).append(d)
            for seq, ds in by_seq.items():
                focus = any(d.options.when_focus_on for d in ds)
                print(f"{mode}\t{'>'.join(name(k) for k in seq)}\t{'focus' if focus else ''}")


main()
