"""Protocol, readiness, and diagnostic checks; not native compositor evidence."""

import copy
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import live_discovery as discovery


class DiscoveryStatusTest(unittest.TestCase):
    def metadata(self, production=False):
        value = {"state": "discovery_only", "abi": {"match": True},
                 "capabilities": {"supported": ["discovery"], "enabled": ["discovery"]},
                 "transport": {"ready": True}, "compositor_epoch": 42,
                 "protocol": {"major": 2, "minor": 0, "max_frame_bytes": 4120}}
        if production:
            lane = {"held_observer": True, "held_cleanup_unconfirmed": False,
                    "foreground_cleanup_pending": False, "held_button": 0,
                    "held_keys": 0, "lease_active": False, "drag_active": False}
            value.update(state="input_v3_candidate", input={
                "protocol": 3, "test_only": False, "source_guard_version": 1,
                "held_phase_version": 1, "upgrade": "desktop_restart",
                "lanes": [lane.copy(), lane.copy()]})
        return value

    def observe(self, value):
        with patch.object(discovery, "hyprctl", return_value=json.dumps(value)) as query:
            result = discovery.status()
        query.assert_called_once_with("-j", "cua:status")
        return result

    def rejected(self, value):
        with self.assertRaises((AssertionError, KeyError, TypeError)):
            self.observe(value)

    def test_discovery_and_experiment_tiers_remain_unchanged(self):
        value = self.metadata()
        self.assertEqual(self.observe(value), value)
        value.update(state="input_experiment", experiment={"test_only": True})
        self.assertEqual(self.observe(value), value)
        value["experiment"]["test_only"] = False
        self.rejected(value)
        value = self.metadata()
        value["state"] = "input_v3_candidate"
        self.rejected(value)

    def test_idle_production_tier_passes(self):
        value = self.metadata(production=True)
        self.assertEqual(self.observe(value), value)

    def test_missing_or_incompatible_production_fields_fail(self):
        original = self.metadata(production=True)
        for key, bad in (("protocol", 2), ("protocol", 4), ("test_only", True),
                         ("source_guard_version", 0), ("source_guard_version", 2),
                         ("held_phase_version", 0), ("held_phase_version", 2),
                         ("upgrade", "hot_reload"), ("lanes", []),
                         ("lanes", original["input"]["lanes"][:1]),
                         ("lanes", original["input"]["lanes"] * 2)):
            with self.subTest(field=key, bad=bad):
                value = copy.deepcopy(original)
                value["input"][key] = bad
                self.rejected(value)
                del value["input"][key]
                self.rejected(value)
        for state in ("discovery_only", "input_experiment"):
            value = copy.deepcopy(original)
            value["state"] = state
            self.rejected(value)

    def test_each_lane_requires_observer_no_debt_and_no_active_hold(self):
        original = self.metadata(production=True)
        for index in (0, 1):
            for key, bad in (("held_observer", False), ("held_observer", 1),
                             ("held_cleanup_unconfirmed", True),
                             ("foreground_cleanup_pending", True),
                             ("held_button", 1), ("held_keys", 1),
                             ("lease_active", True), ("drag_active", True)):
                with self.subTest(lane=index, field=key, bad=bad):
                    value = copy.deepcopy(original)
                    value["input"]["lanes"][index][key] = bad
                    self.rejected(value)
                    del value["input"]["lanes"][index][key]
                    self.rejected(value)

    def test_all_tiers_retain_abi_capability_transport_and_v2_framing_checks(self):
        for tier in ("discovery", "experiment", "production"):
            original = self.metadata(tier == "production")
            if tier == "experiment":
                original.update(state="input_experiment", experiment={"test_only": True})
            for path, bad in ((("abi", "match"), False),
                              (("capabilities", "supported"), ["discovery", "input"]),
                              (("capabilities", "enabled"), ["discovery", "input"]),
                              (("transport", "ready"), False),
                              (("protocol", "major"), 3), (("protocol", "minor"), 1),
                              (("protocol", "max_frame_bytes"), 8192),
                              (("compositor_epoch",), 0)):
                with self.subTest(tier=tier, field=path):
                    value = copy.deepcopy(original)
                    parent = value if len(path) == 1 else value[path[0]]
                    parent[path[-1]] = bad
                    self.rejected(value)
                    del parent[path[-1]]
                    self.rejected(value)


class DiscoveryReadinessTest(unittest.TestCase):
    def test_requires_consecutive_equal_samples(self):
        first = {"cursor": {"x": 0, "y": 0}}
        settled = {"cursor": {"x": 100, "y": 100}}
        with patch.object(discovery, "desktop_state", side_effect=[first, first, settled, settled, settled]) as observe, \
                patch.object(discovery.time, "sleep"):
            self.assertEqual(discovery.stable_desktop_state(), settled)
        self.assertEqual(observe.call_count, 5)

    def test_unstable_preflight_fails(self):
        with patch.object(discovery, "desktop_state", side_effect=[{"cursor": 1}, {"cursor": 2}]), \
                patch.object(discovery.time, "monotonic", side_effect=[0, 0, 1, 6]), \
                patch.object(discovery.time, "sleep"):
            with self.assertRaisesRegex(TimeoutError, "before discovery requests"):
                discovery.stable_desktop_state()

    def test_observation_failure_is_not_swallowed(self):
        with patch.object(discovery, "desktop_state", side_effect=RuntimeError("query failed")):
            with self.assertRaisesRegex(RuntimeError, "query failed"):
                discovery.stable_desktop_state()

    def test_post_request_change_still_fails_with_sanitized_delta(self):
        before = {"active_window": "private-address", "active_workspace": 1,
                  "cursor": {"x": 10, "y": 20}, "windows": [("private-window",)]}
        after = {**before, "cursor": {"x": 11, "y": 20}}
        with self.assertRaises(AssertionError) as result:
            discovery.assert_unchanged(before, after)
        message = str(result.exception)
        self.assertIn('"changed_fields": ["cursor"]', message)
        self.assertIn('"x": 11', message)
        self.assertNotIn("private-address", message)
        self.assertNotIn("private-window", message)
        discovery.assert_unchanged(before, before)


class DiscoveryTest(unittest.TestCase):
    def test_exchange_sends_and_correlates_request(self):
        packet = discovery.HEADER.pack(b"CUA2", 2, 0, 3, 0, 17, 0)
        client = Mock()
        client.send.return_value = len(packet)
        client.recv.return_value = discovery.HEADER.pack(b"CUA2", 2, 0, 4, 0, 17, 0)
        self.assertEqual(discovery.exchange(client, 3, 17), (4, b""))
        client.send.assert_called_once_with(packet)

    def test_exchange_rejects_short_send_before_receive(self):
        client = Mock()
        client.send.return_value = 0
        with self.assertRaises(AssertionError):
            discovery.exchange(client, 3, 17)
        client.recv.assert_not_called()

    def test_exchange_rejects_wrong_request_id(self):
        client = Mock()
        client.send.return_value = discovery.HEADER.size
        client.recv.return_value = discovery.HEADER.pack(b"CUA2", 2, 0, 4, 0, 18, 0)
        with self.assertRaises(AssertionError):
            discovery.exchange(client, 3, 17)

    def test_optimized_python_refuses_before_cli_or_compositor_access(self):
        script = str(Path(discovery.__file__).resolve())
        for options, optimize in ((["-O"], ""), ([], "1"), (["-OO"], "")):
            with self.subTest(options=options, optimize=optimize):
                result = subprocess.run(
                    [sys.executable, *options, script, "--help"],
                    env={**os.environ, "PYTHONOPTIMIZE": optimize},
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Discovery validation requires assertions", result.stderr)
                self.assertNotIn("usage:", result.stdout)


if __name__ == "__main__":
    unittest.main()
