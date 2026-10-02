#!/usr/bin/env python3
"""Disposable native fullscreen color/event oracle; never changes output configuration."""
import argparse
import json
import time

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, GLib, Gtk

parser = argparse.ArgumentParser()
parser.add_argument("--output", required=True)
parser.add_argument("--journal", required=True)
args = parser.parse_args()
journal = open(args.journal, "x", buffering=1)
screen = Gdk.Screen.get_default()
matches = [i for i in range(screen.get_n_monitors())
           if screen.get_monitor_plug_name(i) == args.output]
if len(matches) != 1:
    raise RuntimeError("exact GDK output identity unavailable: " + args.output)

state = {"clicks": 0, "scrolls": 0}


def record(kind, **values):
    journal.write(json.dumps({"kind": kind, "time": time.monotonic_ns(), **values}) + "\n")


window = Gtk.Window(title="Cua Dual Display " + args.output)
window.set_decorated(False)
canvas = Gtk.DrawingArea()
canvas.add_events(Gdk.EventMask.ALL_EVENTS_MASK)
window.add(canvas)


def draw(widget, cr):
    # Static and output-distinct, so independent grim captures can be compared
    # without temporal text, animation, counters or user content.
    colors = ((0.12, 0.32, 0.70), (0.85, 0.20, 0.10))
    cr.set_source_rgb(*colors[args.output == "DP-1"])
    cr.paint()
    cr.set_source_rgb(0.2, 0.8, 0.3)
    cr.rectangle(0, 0, widget.get_allocated_width() / 2,
                 widget.get_allocated_height() / 2)
    cr.fill()
    return False


def event(widget, e):
    if e.type in (Gdk.EventType.BUTTON_PRESS, Gdk.EventType.BUTTON_RELEASE,
                  Gdk.EventType.MOTION_NOTIFY, Gdk.EventType.SCROLL):
        if e.type == Gdk.EventType.BUTTON_RELEASE:
            state["clicks"] += 1
        if e.type == Gdk.EventType.SCROLL:
            state["scrolls"] += 1
        record(e.type.value_nick, x=e.x, y=e.y, **state)
    return False


canvas.connect("draw", draw)
canvas.connect("event", event)
window.connect("destroy", Gtk.main_quit)
window.fullscreen_on_monitor(screen, matches[0])
window.show_all()


def heartbeat():
    record("state", width=canvas.get_allocated_width(),
           height=canvas.get_allocated_height(), **state)
    return True


GLib.timeout_add(100, heartbeat)
Gtk.main()
journal.close()
