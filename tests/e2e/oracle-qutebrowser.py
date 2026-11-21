# qutebrowser's own bindings for tests/e2e/oracle.py, run inside it by `:debug-pyeval --file` at startup: one
# MODE<TAB>KEY<TAB>COMMAND line per binding, the modes and keys as qute://bindings lists them
# (browser/qutescheme.py:459-473 at v3.7.0), each key as qutebrowser prints it (str(KeySequence)).
import sys
from qutebrowser.config import config

modes = set(config.val.bindings.default).union(config.val.bindings.commands)
modes.discard("normal")
for mode in ["normal"] + sorted(modes):
    for key, command in config.key_instance.get_bindings_for(mode).items():
        print(f"{mode}\t{key}\t{command}")
sys.stdout.flush()
