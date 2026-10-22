"""Disposable native splash/main/child/late-modal oracle; never a product hook."""
import argparse
import json
import os
import subprocess
import sys

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import GLib, Gtk

parser = argparse.ArgumentParser()
parser.add_argument("--journal", required=True)
parser.add_argument("--role", default="splash", choices=["splash", "child", "sentinel"])
args = parser.parse_args()
windows = []
children = []


def window(role, parent=None):
    widget = Gtk.Window(title="CUA hidden launch " + role)
    widget.set_default_size(480, 320)
    if parent:
        widget.set_transient_for(parent)
        widget.set_modal(True)
    widget.add(Gtk.Label(label=role))
    widget.show_all()
    windows.append(widget)
    with open(args.journal, "a", encoding="utf-8") as journal:
        journal.write(json.dumps({"pid": os.getpid(), "role": role}) + "\n")
    return widget


initial = window(args.role)
if args.role == "sentinel":
    def focus_out(_widget, _event):
        with open(args.journal, "a", encoding="utf-8") as journal:
            journal.write(json.dumps({"event": "focus-out", "pid": os.getpid()}) + "\n")
        return False
    initial.connect("focus-out-event", focus_out)
if args.role == "splash":
    def main():
        initial.destroy()
        parent = window("main")
        children.append(subprocess.Popen([
            sys.executable, __file__, "--journal", args.journal, "--role", "child"
        ]))
        # Past the historical eight-second startup-only guard. This must be
        # protected by process lifetime, not a lucky startup sweep.
        GLib.timeout_add(12000, lambda: (window("late-modal", parent), False)[1])
        return False
    GLib.timeout_add(1200, main)
Gtk.main()
