# LibreOffice's own shortcuts for tests/e2e/oracle.py: a headless soffice on this home's profile, asked over UNO for
# Writer's keys as LibreOffice resolves them (the module's, then each global key on an event the module doesn't bind,
# as svtools' AcceleratorExecute::impl_ts_findCommand), each labelled as UICommandDescription names it. One
# KEYCODE<TAB>MODIFIERS<TAB>COMMAND<TAB>LABEL line per key (css.awt.Key, css.awt.KeyModifier). soffice is terminated
# at the end; the harness checks none survives.
import os
import signal
import subprocess
import sys
import time

import uno
from com.sun.star.container import NoSuchElementException

MODULE = "com.sun.star.text.TextDocument"
PIPE = "kbf-e2e-oracle"

soffice = subprocess.Popen(["soffice", "--headless", "--invisible", "--norestore", "--nologo", "--nodefault",
                            f"--accept=pipe,name={PIPE};urp;"], stdout=subprocess.DEVNULL, start_new_session=True)


def kill():
    """SIGKILL soffice's whole process group: oosplash and the soffice.bin it starts."""
    try:
        os.killpg(soffice.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    soffice.wait()


local = uno.getComponentContext()
resolver = local.ServiceManager.createInstanceWithContext("com.sun.star.bridge.UnoUrlResolver", local)
ctx = None
for _ in range(200):
    if soffice.poll() is not None:
        sys.exit(f"oracle-libreoffice.py: soffice exited ({soffice.returncode}) before answering on its pipe")
    try:
        ctx = resolver.resolve(f"uno:pipe,name={PIPE};urp;StarOffice.ComponentContext")
        break
    except Exception:
        time.sleep(0.2)
if ctx is None:
    kill()
    sys.exit("oracle-libreoffice.py: soffice never answered on its pipe")
smgr = ctx.ServiceManager
try:
    supplier = smgr.createInstanceWithContext("com.sun.star.ui.ModuleUIConfigurationManagerSupplier", ctx)
    module = supplier.getUIConfigurationManager(MODULE).getShortCutManager()
    glob = smgr.createInstanceWithContext("com.sun.star.ui.GlobalAcceleratorConfiguration", ctx)
    labels = ctx.getValueByName("/singletons/com.sun.star.frame.theUICommandDescription").getByName(MODULE)

    def label(command):
        try:
            props = {p.Name: p.Value for p in labels.getByName(command)}
        except NoSuchElementException:
            return command
        return props.get("Name") or command

    seen = set()
    for config in (module, glob):
        for ev in config.getAllKeyEvents():
            k = (ev.KeyCode, ev.Modifiers)
            if k in seen:
                continue
            seen.add(k)
            command = config.getCommandByKeyEvent(ev)
            print(f"{ev.KeyCode}\t{ev.Modifiers}\t{command}\t{label(command)}")
    sys.stdout.flush()
finally:
    try:
        smgr.createInstanceWithContext("com.sun.star.frame.Desktop", ctx).terminate()
    except Exception:
        pass
    try:
        soffice.wait(timeout=20)
    except subprocess.TimeoutExpired:
        kill()
