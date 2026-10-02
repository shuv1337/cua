"""Exercise only the extracted callback; never initialize GTK or create windows."""
import ast
import enum
import sys
import types

tree = ast.parse(sys.stdin.read())
callback = next(node for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name == "event")
code = compile(ast.Module(body=[callback], type_ignores=[]), "<fixture-event>", "exec")


class EventType(enum.Enum):
    BUTTON_PRESS = "button-press"
    BUTTON_RELEASE = "button-release"
    MOTION_NOTIFY = "motion-notify"
    SCROLL = "scroll"

    @property
    def value_nick(self):
        return self.value


records = []
state = {"clicks": 0, "scrolls": 0}
namespace = {"Gdk": types.SimpleNamespace(EventType=EventType), "state": state,
             "record": lambda kind, **values: records.append({"kind": kind, **values})}
exec(code, namespace)
event = namespace["event"]


def packet(kind, ok=True, button=1):
    return types.SimpleNamespace(type=kind, x=100.0, y=720.0,
                                 button=types.SimpleNamespace(button=button),
                                 get_button=lambda: (ok, button))


for kind in (EventType.BUTTON_PRESS, EventType.BUTTON_RELEASE):
    assert event(None, packet(kind)) is False
    assert records[-1]["button"] == 1 and type(records[-1]["button"]) is int
    assert (records[-1]["x"], records[-1]["y"]) == (100.0, 720.0)
assert state == {"clicks": 1, "scrolls": 0}
for ok, button in ((False, 1), (True, None), (True, "1"), (True, True), (True, 0)):
    for kind in (EventType.BUTTON_PRESS, EventType.BUTTON_RELEASE):
        assert event(None, packet(kind, ok, button)) is False
        assert records[-1] == {"kind": "fixture-event-error",
                               "code": "invalid-button-payload", "event_type": kind.value_nick}
        assert state == {"clicks": 1, "scrolls": 0}
event(None, packet(EventType.MOTION_NOTIFY))
assert records[-1]["button"] is None
scroll = packet(EventType.SCROLL)
scroll.direction = types.SimpleNamespace(value_nick="down")
scroll.get_scroll_deltas = lambda: (False, 0.0, 0.0)
scroll.is_scroll_stop_event = lambda: False
event(None, scroll)
assert records[-1]["button"] is None and records[-1]["direction"] == "down"
assert state == {"clicks": 1, "scrolls": 1}
print("mock callback: numeric press/release, invalid-payload refusal, motion/scroll OK")

if "--pygobject" in sys.argv:
    import gi
    gi.require_version("Gdk", "3.0")
    from gi.repository import Gdk
    namespace["Gdk"] = Gdk
    state.update(clicks=0, scrolls=0)
    for kind in (Gdk.EventType.BUTTON_PRESS, Gdk.EventType.BUTTON_RELEASE):
        native = Gdk.Event.new(kind)
        native.button.button = 1
        native.button.x, native.button.y = 100.0, 720.0
        assert native.get_button() == (True, 1)
        assert event(None, native) is False
        assert records[-1]["button"] == 1 and type(records[-1]["button"]) is int
        assert (records[-1]["x"], records[-1]["y"]) == (100.0, 720.0)
    assert state == {"clicks": 1, "scrolls": 0}
    print("real Gdk3 event construction/callback: press/release OK; no GUI initialization")
