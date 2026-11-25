#!/usr/bin/python3.10
"""Tests on a virtual GNOME display (docs/LINUX.md): plays the person at the PC who approves GNOME's screen-sharing or
remote-desktop dialog (xdg-desktop-portal-gnome), through the accessibility interface (AT-SPI), as a screen reader user
would. Nothing in Queqiao goes round GNOME's dialog; only the tests answer it this way.

Usage: portal_approve.py [seconds] [--remember] [--list]   (waits up to `seconds`, 20 by default, for one dialog;
--list also prints the dialog's controls)
A remote-desktop dialog's "Allow remote interaction" switch is turned on, as a person letting Queqiao control the PC
does (else GNOME shares the screen only). --remember also ticks "Remember this selection" (or the like) where the
dialog has one, as a person allowing it for good would. Prints APPROVED <dialog> or APPROVE_FAIL <why>.
"""
import subprocess
import sys
import time

# pyatspi is imported by the one-shot pass only (once): the waiting process stays off the accessibility bus.
pyatspi = None

BUTTONS = ("Share", "Allow")
# Matched in lower case: GNOME 42 says "Allow remote interaction" (a switch), 46 "Allow Remote Interaction" (a check box).
REMEMBER = ("remember",)
INTERACTION = ("remote interaction", "allow remote control")


def buttons(node, found):
    try:
        role = node.getRoleName()
        if role in ("push button", "check box", "toggle button"):
            found.append(node)
        for index in range(node.childCount):
            buttons(node.getChildAtIndex(index), found)
    except Exception:
        pass
    return found


def label(node):
    name = node.name or ""
    # A switch is named by its row ("Allow remote interaction"), not by itself ("GtkSwitch").
    if name.startswith("Gtk"):
        name = ""
        ancestor = node.parent
        for _ in range(5):
            if ancestor is None:
                break
            if ancestor.name and not ancestor.name.startswith("Gtk"):
                return ancestor.name
            ancestor = ancestor.parent
    if not name:
        for index in range(node.childCount):
            child = node.getChildAtIndex(index)
            if child.getRoleName() == "label" and child.name:
                return child.name
    return name.replace("_", "")


def checked(node):
    """Whether a switch or check box is on now (not as AT-SPI cached it)."""
    try:
        node.clearCache()
    except Exception:
        pass
    return node.getState().contains(pyatspi.STATE_CHECKED)


def click(node):
    action = node.queryAction()
    for index in range(action.nActions):
        if action.getName(index) in ("click", "press", "activate", "toggle"):
            return action.doAction(index)
    return action.doAction(0)


def main():
    """Waits for a dialog: a fresh one-shot pass each half second. A process that was already waiting when the dialog
    opened can press its buttons but not switch its switch (seen with GNOME 42 on cc4-1), one started after can."""
    if "--once" in sys.argv:
        return once("--remember" in sys.argv)
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else 20.0
    extra = [flag for flag in ("--remember", "--list") if flag in sys.argv]
    until = time.time() + seconds
    while time.time() < until:
        result = subprocess.run([sys.executable, __file__, "--once", *extra], capture_output=True, text=True)
        if result.returncode != 2:
            print(result.stdout.strip())
            return result.returncode
        time.sleep(0.5)
    print("APPROVE_FAIL no portal dialog")
    return 1


def once(remember):
    """One look for a dialog: 0 approved, 2 none open."""
    global pyatspi
    import pyatspi as module

    pyatspi = module
    desktop = pyatspi.Registry.getDesktop(0)
    for app_index in range(desktop.childCount):
        app = desktop.getChildAtIndex(app_index)
        if app is None or "portal-gnome" not in (app.name or ""):
            continue
        for window_index in range(app.childCount):
            window = app.getChildAtIndex(window_index)
            if window is None or window.getRoleName() not in ("frame", "dialog"):
                continue
            # A dialog just shown takes a moment before it answers (as a person takes to read it).
            time.sleep(2)
            found = buttons(window, [])
            if "--list" in sys.argv:
                # What the dialog offers (each desktop version words and builds it its own way).
                for node in found:
                    print(f"control {node.getRoleName()} {label(node)!r} checked={checked(node)}")
            for node in found:
                if node.getRoleName() != "push button" and any(word in label(node).lower() for word in INTERACTION):
                    # Clicked while it reads off, the state read afresh each time (AT-SPI caches states).
                    on = False
                    for _ in range(3):
                        on = checked(node)
                        if on:
                            break
                        click(node)
                        time.sleep(1)
                    print(f"switch {label(node)!r}: {'on' if on else 'still off'}")
            if remember:
                for node in found:
                    if node.getRoleName() != "push button" and any(word in label(node).lower() for word in REMEMBER):
                        states = node.getState()
                        if not states.contains(pyatspi.STATE_CHECKED):
                            click(node)
            for node in found:
                if node.getRoleName() == "push button" and label(node).split()[-1] in BUTTONS:
                    time.sleep(0.3)
                    click(node)
                    print(f"APPROVED {window.name} ({label(node)})")
                    return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
