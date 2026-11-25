#!/usr/bin/python3.10
"""Tests on a virtual Wayland display (docs/LINUX.md): a native Wayland window (GTK 4) that covers the screen, white,
and logs what reaches it from the compositor, one line each, flushed at once:
    motion <x> <y>          the pointer, also where it entered (window positions; full screen: screen positions)
    button <n> <pressed|released>
    scroll <dx> <dy>
    key <code> <pressed|released>   the hardware key code (Linux input code + 8, as X11 numbers keys)
    active <True|False>     whether the window has the keyboard
Usage: GDK_BACKEND=wayland wayland_test_window.py <log file> [seconds]
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402

LOG = open(sys.argv[1], "w", buffering=1)
SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 60


def log(text):
    LOG.write(text + "\n")


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="Queqiao test window")
    css = Gtk.CssProvider()
    css.load_from_data(b"window, box { background: #ffffff; }")
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    window.set_child(Gtk.Box())
    window.connect("notify::is-active", lambda w, _p: log(f"active {w.is_active()}"))
    motion = Gtk.EventControllerMotion()
    motion.connect("enter", lambda _c, x, y: log(f"motion {x:.0f} {y:.0f}"))
    motion.connect("motion", lambda _c, x, y: log(f"motion {x:.0f} {y:.0f}"))
    window.add_controller(motion)
    click = Gtk.GestureClick(button=0)
    click.connect("pressed", lambda g, _n, _x, _y: log(f"button {g.get_current_button()} pressed"))
    click.connect("released", lambda g, _n, _x, _y: log(f"button {g.get_current_button()} released"))
    window.add_controller(click)
    scroll = Gtk.EventControllerScroll(flags=Gtk.EventControllerScrollFlags.BOTH_AXES)
    scroll.connect("scroll", lambda _c, dx, dy: (log(f"scroll {dx:.0f} {dy:.0f}"), False)[1])
    window.add_controller(scroll)
    # In the capture phase, before anything else in the window takes the key.
    keys = Gtk.EventControllerKey()
    keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    keys.connect("key-pressed", lambda _c, _v, code, _s: (log(f"key {code} pressed"), False)[1])
    keys.connect("key-released", lambda _c, _v, code, _s: log(f"key {code} released"))
    window.add_controller(keys)
    window.fullscreen()
    window.present()
    GLib.timeout_add_seconds(SECONDS, app.quit)


app = Gtk.Application(application_id="org.queqiao.TestWindow")
app.connect("activate", activate)
app.run([])
