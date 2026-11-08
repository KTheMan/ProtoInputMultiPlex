"""Independent advanced-graph gate preparation; no live launch until READY.

The unambiguous additive-default/filter/coordinate cases are prepared here.
Queue timing, origin cancellation, overflow and final entry point are added only
after the producer's final runtime contract. This file cannot report acceptance
until those required cases and the separate real GUI gate are complete.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import time

_spec = importlib.util.spec_from_file_location("advanced_graph_helpers", Path(__file__).with_name("test-replication-graph-acceptance.py"))
_graph = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_graph)
eventually = _graph.eventually

DEFAULT_OPTIONS = {"position": "anchored", "offset_x": 0, "offset_y": 0,
    "motion": True, "wheel": True, "horizontal_wheel": True, "buttons": [True] * 5,
    "click_delay_ms": 0, "step_interval_ms": 0}
BUTTON_NAMES = ["primary", "secondary", "middle", "back", "forward"]


def edge(source, target, options=None, **categories):
    value = _graph.edge(source, target, **categories)
    value["options"] = {**copy.deepcopy(DEFAULT_OPTIONS), **(options or {})}
    return value


def count_edges(report, start):
    return [(e["button"], e["pressed"]) for e in report["mouse_events"][start:]]


class AdvancedGate(_graph.GraphGate):
    run_prefix = "acceptance-advanced-replication"

    def __init__(self, args, permuted=False):
        # Deliberately make lexically preferred branch C a later dense index.
        restored = {"ids": ["advanced-source", "advanced-z-branch", "advanced-a-branch", "advanced-dest"],
                    "definition": _graph.definition([])}
        super().__init__(args, restored=restored)
        if permuted:
            self.order = [0, 3, 2, 1]

    def users(self):
        return tuple(self.ids.index(i) for i in self.restored["ids"])

    def reanchor(self):
        self.apply(_graph.definition([]))
        for user, point in enumerate(self.points):
            self.input("move_to", user=user, x=point[0], y=point[1])
        eventually(self.reports, lambda rs: all(r["cursor"] == self.points[i] and not r["held_keys"] and not r["buttons"] for i, r in enumerate(rs)))

    def set_single(self, options=None):
        a, b, _, _ = self.users()
        value = _graph.definition([edge(self.ids[a], self.ids[b], options)])
        self.apply(value)
        assert self.graph()["definition"] == value

    def default_compatibility(self):
        self.reanchor()
        a, b, c, d = self.users()
        # Genuine old v1 JSON omits options; observe its additive defaults.
        self.apply(_graph.definition([_graph.edge(self.ids[a], self.ids[b])]))
        assert self.graph()["definition"] == _graph.definition([edge(self.ids[a], self.ids[b])])
        before = self.reports()
        self.input("text", user=a, text="Default café 🙂")
        self.key(a, "f5", True)
        self.key(a, "f5", False)
        self.input("button", user=a, button="back", pressed=True)
        self.input("button", user=a, button="back", pressed=False)
        self.input("motion", user=a, dx=7, dy=3)
        eventually(self.reports, lambda rs: all(rs[u]["text"] == before[u]["text"] + "Default café 🙂" and rs[u]["cursor"] == [self.points[u][0] + 7, self.points[u][1] + 3] and rs[u]["mouse_event_count"] == before[u]["mouse_event_count"] + 2 and rs[u]["key_event_count"] == before[u]["key_event_count"] + 2 for u in (a, b)))
        rs = self.reports()
        for user in (c, d):
            assert all(rs[user][k] == before[user][k] for k in ("cursor", "text", "mouse_event_count", "key_event_count", "wheel_events"))
        self.released()
        self.snapshot("old-v1-default-options-immediate-anchored-native-delivery")

    def component_filters(self):
        self.reanchor()
        a, b, _, _ = self.users()
        for user in (a, b):
            self.client.tool("routing_settings", {"user": user, "settings": {"lock_window": False, "horizontal_wheel": "convert_to_vertical"}})
        self.set_single({"motion": False, "wheel": False, "horizontal_wheel": True})
        before = self.reports()
        self.input("motion", user=a, dx=5, dy=2)
        self.input("wheel", user=a, delta=120, axis="vertical")
        eventually(self.reports, lambda rs: rs[a]["cursor"] == [self.points[a][0] + 5, self.points[a][1] + 2] and rs[a]["wheel_events"] == before[a]["wheel_events"] + 1)
        rs = self.reports()
        assert rs[b]["cursor"] == before[b]["cursor"] and rs[b]["wheel_events"] == before[b]["wheel_events"]
        self.input("wheel", user=a, delta=120, axis="horizontal")
        eventually(self.reports, lambda rs: rs[a]["wheel_events"] == before[a]["wheel_events"] + 2 and rs[b]["wheel_events"] == before[b]["wheel_events"] + 1)
        assert self.report(b)["wheel_delta"] == before[b]["wheel_delta"] + 120
        self.set_single({"motion": True, "wheel": True, "horizontal_wheel": False})
        before = self.reports()
        self.input("wheel", user=a, delta=-120, axis="horizontal")
        eventually(self.reports, lambda rs: rs[a]["wheel_events"] == before[a]["wheel_events"] + 1)
        assert self.report(b)["wheel_events"] == before[b]["wheel_events"]
        self.input("wheel", user=a, delta=-120, axis="vertical")
        eventually(self.reports, lambda rs: rs[b]["wheel_delta"] == before[b]["wheel_delta"] - 120)
        self.input("motion", user=a, dx=3, dy=-1)
        eventually(self.reports, lambda rs: rs[b]["cursor"] == [before[b]["cursor"][0] + 3, before[b]["cursor"][1] - 1])
        self.snapshot("independent-motion-vertical-and-horizontal-wheel-native-filters")

    def symmetric_buttons(self):
        a, b, _, _ = self.users()
        for button, name in enumerate(BUTTON_NAMES):
            for allowed in (False, True):
                flags = [True] * 5
                flags[button] = allowed
                self.set_single({"buttons": flags})
                before = self.reports()
                self.input("button", user=a, button=name, pressed=True)
                if allowed:
                    eventually(self.reports, lambda rs: all(rs[u]["buttons"] & (1 << button) for u in (a, b)))
                else:
                    eventually(self.reports, lambda rs: rs[a]["buttons"] & (1 << button))
                    assert not self.report(b)["buttons"] & (1 << button)
                self.input("button", user=a, button=name, pressed=False)
                eventually(self.reports, lambda rs: rs[a]["mouse_event_count"] == before[a]["mouse_event_count"] + 2 and not rs[a]["buttons"] and not rs[b]["buttons"] and (not allowed or rs[b]["mouse_event_count"] == before[b]["mouse_event_count"] + 2))
                rs = self.reports()
                assert count_edges(rs[a], before[a]["mouse_event_count"]) == [(button, True), (button, False)]
                assert count_edges(rs[b], before[b]["mouse_event_count"]) == ([(button, True), (button, False)] if allowed else [])
        # Mouse component filters do not suppress key or grouped text categories.
        self.set_single({"motion": False, "wheel": False, "horizontal_wheel": False, "buttons": [False] * 5})
        before = self.reports()
        self.key(a, "f6", True)
        self.key(a, "f6", False)
        self.input("text", user=a, text="filters")
        eventually(self.reports, lambda rs: all(rs[u]["key_event_count"] == before[u]["key_event_count"] + 2 and rs[u]["text"] == before[u]["text"] + "filters" for u in (a, b)))
        self.snapshot("five-button-symmetric-native-filters-key-and-text-unaffected")

    def mixed_coordinates(self):
        a, b, c, _ = self.users()
        for anchored_first in (False, True):
            self.reanchor()
            ab = {} if anchored_first else {"position": "source_relative", "offset_x": self.points[b][0] - self.points[a][0], "offset_y": self.points[b][1] - self.points[a][1]}
            bc = {"position": "source_relative", "offset_x": self.points[c][0] - self.points[b][0], "offset_y": self.points[c][1] - self.points[b][1]}
            self.apply(_graph.definition([edge(self.ids[a], self.ids[b], ab), edge(self.ids[b], self.ids[c], bc)]))
            self.input("move_to", user=a, x=self.points[a][0] + 9, y=self.points[a][1] + 4)
            expected = [[self.points[u][0] + 9, self.points[u][1] + 4] for u in (a, b, c)]
            eventually(self.reports, lambda rs: [rs[u]["cursor"] for u in (a, b, c)] == expected)
            state = self.state()
            assert [[state["users"][u]["overlay"]["x"], state["users"][u]["overlay"]["y"]] for u in (a, b, c)] == expected
        self.snapshot("actual-native-source-relative-and-mixed-chain-per-hop-coordinates")

    def queue(self):
        return self.state()["automation"]["replication"]

    def cancel_replication(self, error=False, revision=None):
        return self.client.tool("routing_graph", {"operation": "cancel", "expected_revision": self.graph()["revision"] if revision is None else revision}, error=error)

    def program(self, user, events, composite=False):
        value = _graph._studio.clip(*events)
        if composite:
            self.client.tool("automation_clip", {"action": "load", "clip": value})
            self.client.tool("automation_play", {"loops": 1, "speed": 1.})
        else:
            self.client.tool("macro_studio", {"operation": "load", "user": user, "clip": value})
            self.client.tool("macro_studio", {"operation": "play", "user": user, "options": {"loops": 1}})

    def native_keys(self, user, start):
        return [(e["vk"], e["pressed"]) for e in self.report(user)["key_events"][start:]]

    def calibrate_clocks(self, source, target):
        before = self.reports()
        self.key(source, "f6", True)
        self.key(source, "f6", False)
        eventually(self.reports, lambda rs: all(rs[u]["key_event_count"] == before[u]["key_event_count"] + 2 for u in (source, target)))
        stamps = []
        for user in (source, target):
            events = self.report(user)["native_events"][before[user]["native_event_count"]:]
            stamps.append(next(e["at_us"] for e in events if e["kind"] == "key" and e["code"] == 117 and e["pressed"]))
        return stamps[1] - stamps[0]

    def canonical(self, reverse=False):
        self.reanchor()
        a, b, c, d = self.users()
        relative = lambda source, target, extra=0, delay=0: {"position": "source_relative", "offset_x": self.points[target][0] - self.points[source][0] + extra, "offset_y": self.points[target][1] - self.points[source][1], "click_delay_ms": delay}
        ac = relative(a, c, extra=11, delay=220)
        ac["buttons"] = [True, True, True, False, True]
        edges = [edge(self.ids[a], self.ids[b], relative(a, b, delay=80)), edge(self.ids[a], self.ids[c], ac),
                 edge(self.ids[b], self.ids[d], relative(b, d, delay=80)), edge(self.ids[c], self.ids[d], relative(c, d, delay=220))]
        if reverse:
            edges.reverse()
        self.apply(_graph.definition(edges))
        clock_shift = self.calibrate_clocks(a, d)
        self.input("move_to", user=a, x=self.points[a][0] + 6, y=self.points[a][1] + 2)
        expected = {a: [self.points[a][0] + 6, self.points[a][1] + 2], b: [self.points[b][0] + 6, self.points[b][1] + 2], c: [self.points[c][0] + 17, self.points[c][1] + 2], d: [self.points[d][0] + 17, self.points[d][1] + 2]}
        eventually(self.reports, lambda rs: all(rs[u]["cursor"] == p for u, p in expected.items()))
        before = self.reports()
        for name in ("primary", "back"):
            self.input("button", user=a, button=name, pressed=True)
            self.input("button", user=a, button=name, pressed=False)
        eventually(self.reports, lambda rs: rs[d]["mouse_event_count"] == before[d]["mouse_event_count"] + 4)
        rows = self.report(d)["native_events"][before[d]["native_event_count"]:]
        buttons = [e for e in rows if e["kind"] == "button"]
        assert [(e["code"], e["pressed"]) for e in buttons] == [(3, True), (3, False), (0, True), (0, False)]
        source_events = [e for e in self.report(a)["native_events"][before[a]["native_event_count"]:] if e["kind"] == "button"]
        delay_ms = {}
        for button, lower in ((0, 350), (3, 90)):
            source_stamp = next(e["at_us"] for e in source_events if e["code"] == button and e["pressed"])
            target_stamp = next(e["at_us"] for e in buttons if e["code"] == button and e["pressed"])
            delay_ms[str(button)] = (target_stamp - source_stamp - clock_shift) / 1000
            assert lower <= delay_ms[str(button)] < 3000, delay_ms
        self.evidence["canonical_result"] = {"relative_destination_delta": [17, 2], "delay_ms": delay_ms, "index_order": self.order, "json_reversed": reverse}
        self.released()
        self.snapshot("stable-id-canonical-per-component-diamond-native-coordinate-and-delay-choice")

    def pacing(self):
        self.reanchor()
        a, b, _, _ = self.users()
        self.set_single({"step_interval_ms": 250})
        before = self.reports()
        s = _graph._studio
        self.program(a, [s.event(0, s.key(a, True)), s.event(0, s.key(a, False)), s.event(0, s.key(a, True, vk=117, scan=64)), s.event(0, s.key(a, False, vk=117, scan=64))])
        eventually(self.reports, lambda rs: rs[b]["key_event_count"] == before[b]["key_event_count"] + 4)
        events = [e for e in self.report(b)["native_events"][before[b]["native_event_count"]:] if e["kind"] == "key"]
        assert [(e["code"], e["pressed"]) for e in events] == [(116, True), (116, False), (117, True), (117, False)]
        intervals = [(y["at_us"] - x["at_us"]) / 1000 for x, y in zip(events, events[1:])]
        assert all(180 <= gap < 3000 for gap in intervals), intervals
        eventually(self.queue, lambda q: not q["pending_events"])
        eventually(self.state, lambda v: v["macro_studio"]["users"][a]["playback"]["state"] == "completed")
        self.evidence["paced_native_intervals_ms"] = intervals
        self.released()
        self.snapshot("paced-discrete-native-order-and-real-time-spacing-completed-tail")
        self.reanchor()
        self.set_single({"step_interval_ms": 200})
        before = self.reports()
        q_before = self.queue()
        # A whole burst is processed in two bounded ticks; its latest goal is
        # retained in one stream rather than allocating one job per motion.
        self.program(a, [s.event(0, s.move(a, [self.points[a][0] + i, self.points[a][1]])) for i in range(1, 51)])
        queued = eventually(self.queue, lambda q: q["coalesced"] > q_before["coalesced"])
        assert queued["pending_motion"] <= 1 and queued["pending_events"] <= 1 and queued["pending_bytes"] <= 128
        eventually(self.reports, lambda rs: rs[b]["cursor"] == [self.points[b][0] + 50, self.points[b][1]], 6)
        motions = [e for e in self.report(b)["native_events"][before[b]["native_event_count"]:] if e["kind"] == "motion"]
        assert 1 < len({tuple(e["client"]) for e in motions}) < 20
        eventually(self.queue, lambda q: not q["pending_events"])
        self.snapshot("paced-motion-coalesces-bounded-native-progress-final-exact-no-gain")
        self.reanchor()
        self.set_single({"step_interval_ms": 300})
        before = self.reports()
        self.program(a, [s.event(0, s.move(a, [self.points[a][0] + 40, self.points[a][1]])), s.event(0, s.key(a, True)), s.event(0, s.key(a, False))])
        eventually(self.reports, lambda rs: rs[b]["key_event_count"] == before[b]["key_event_count"] + 2)
        trace = self.report(b)["native_events"][before[b]["native_event_count"]:]
        key_index = next(i for i, e in enumerate(trace) if e["kind"] == "key" and e["pressed"])
        assert any(e["kind"] == "motion" for e in trace[:key_index])
        assert self.report(b)["cursor"] == [self.points[b][0] + 40, self.points[b][1]]
        self.snapshot("discrete-barrier-flushes-final-paced-motion-before-native-key")

    def queue_atomicity(self):
        self.reanchor()
        a, b, c, _ = self.users()
        self.set_single({"click_delay_ms": 5000})
        self.key(b, "f5", True)
        self.input("button", user=a, button="back", pressed=True)
        before = self.graph()
        q_before = eventually(self.queue, lambda q: q["pending_events"] == 1)
        # Worker acknowledgement precedes the owned window thread's posted
        # messages/report write. Establish the real hold baseline first.
        eventually(self.reports, lambda rs: 116 in rs[b]["held_keys"] and rs[a]["buttons"] & 8)
        bad_options = [{"click_delay_ms": 60001}, {"step_interval_ms": 1001}, {"offset_x": 1}, {"position": "source_relative", "offset_x": 32768}]
        invalid = [_graph.definition([edge(self.ids[a], self.ids[b], o)]) for o in bad_options]
        invalid += [_graph.definition([edge(self.ids[a], self.ids[b], {"click_delay_ms": 40000}), edge(self.ids[b], self.ids[c], {"click_delay_ms": 40000})])]
        for candidate in invalid:
            self.apply(candidate, error=True)
            assert self.graph() == before
            q = self.queue()
            assert all(q[k] == q_before[k] for k in ("pending_events", "pending_motion", "pending_bytes", "cancelled", "emitted", "suspended"))
            assert 116 in self.report(b)["held_keys"] and self.report(a)["buttons"] & 8
        self.apply(before["definition"], error=True, revision=before["revision"] - 1)
        self.cancel_replication(error=True, revision=before["revision"] - 1)
        assert self.queue()["pending_events"] == 1
        self.cancel_replication()
        assert self.graph() == before and not self.queue()["pending_events"]
        assert 116 in self.report(b)["held_keys"] and self.report(a)["buttons"] & 8
        self.input("button", user=a, button="back", pressed=False)
        self.cancel_replication()
        self.key(b, "f5", False)
        self.released()
        self.snapshot("invalid-options-path-limits-stale-apply-and-cancel-preserve-queue-native-holds")

    def origin_cancel(self):
        a, b, _, _ = self.users()
        s = _graph._studio
        for origin in ("manual", "studio", "composite"):
            self.reanchor()
            self.set_single({"step_interval_ms": 250, "click_delay_ms": 5000})
            if origin != "composite":
                self.key(b, "f5", True)
                self.input("button", user=b, button="back", pressed=True)
            if origin == "manual":
                self.key(a, "f5", True)
                self.input("button", user=a, button="back", pressed=True)
            else:
                events = [s.event(0, s.key(a, True)), s.event(0, s.button(a, True, code=3))]
                # Legacy Play globally retires manual holds and later manual
                # input overrides it. Its B hold is an original Composite
                # action, not an invented coexisting manual hold.
                if origin == "composite":
                    events += [s.event(0, s.key(b, True)), s.event(0, s.button(b, True, code=3))]
                events += [s.event(20_000_000, s.key(a, False)), s.event(20_000_000, s.button(a, False, code=3))]
                if origin == "composite":
                    events += [s.event(20_000_000, s.key(b, False)), s.event(20_000_000, s.button(b, False, code=3))]
                self.program(a, events, composite=origin == "composite")
            eventually(self.reports, lambda rs: all(116 in rs[u]["held_keys"] and rs[u]["buttons"] & 8 for u in (a, b)))
            q = eventually(self.queue, lambda q: q["pending_events"] > 0)
            before = self.graph()
            if origin != "manual":
                self.apply(before["definition"], error=True)
                assert self.queue()["pending_events"] == q["pending_events"] and self.graph() == before
            self.cancel_replication()
            assert self.graph() == before and not self.queue()["pending_events"]
            eventually(self.reports, lambda rs: all(116 in rs[u]["held_keys"] and rs[u]["buttons"] & 8 for u in (a, b)))
            if origin == "manual":
                self.key(a, "f5", False)
                self.input("button", user=a, button="back", pressed=False)
                self.cancel_replication()
            elif origin == "studio":
                self.client.tool("macro_studio", {"operation": "cancel", "user": a})
            else:
                # Legacy global Cancel intentionally retires all holds.
                self.client.tool("automation_cancel")
            if origin != "composite":
                assert 116 in self.report(b)["held_keys"] and self.report(b)["buttons"] & 8
                self.key(b, "f5", False)
                self.input("button", user=b, button="back", pressed=False)
            self.released()
            self.snapshot("cancel-replication-preserves-original-" + origin + "-and-other-native-owners")

    def scoped_program_queue_cancel(self):
        self.reanchor()
        a, b, c, d = self.users()
        s = _graph._studio
        self.apply(_graph.definition([edge(self.ids[a], self.ids[d], {"click_delay_ms": 2000}), edge(self.ids[b], self.ids[c], {"click_delay_ms": 2000})]))
        self.key(d, "f5", True)
        self.program(a, [s.event(0, s.button(a, True, code=3)), s.event(20_000_000, s.button(a, False, code=3))])
        self.program(b, [s.event(0, s.button(b, True, code=4)), s.event(20_000_000, s.button(b, False, code=4))])
        eventually(self.queue, lambda q: q["pending_events"] == 2)
        self.client.tool("macro_studio", {"operation": "cancel", "user": a})
        assert self.queue()["pending_events"] == 1
        eventually(self.reports, lambda rs: rs[c]["buttons"] & 16)
        assert not self.report(d)["buttons"] and 116 in self.report(d)["held_keys"]
        self.client.tool("macro_studio", {"operation": "cancel", "user": b})
        eventually(self.reports, lambda rs: not any(r["buttons"] for r in rs))
        assert 116 in self.report(d)["held_keys"]
        self.key(d, "f5", False)
        self.released()
        self.snapshot("scoped-program-cancel-removes-own-pending-origin-only-other-queue-delivers")

    def overflow(self, release=False):
        self.reanchor()
        a, b, c, d = self.users()
        s = _graph._studio
        edges = [edge(self.ids[a], self.ids[b if not release else d], {"click_delay_ms": 60000})]
        if release:
            edges.append(edge(self.ids[b], self.ids[c], {"click_delay_ms": 60000}))
        self.apply(_graph.definition(edges))
        self.key(d, "f5", True)
        before = self.reports()
        q_before = self.queue()
        assert q_before["capacity"] == 4096 and q_before["byte_capacity"] == 8 * 1024 * 1024
        events = [s.event(0, s.button(a, i % 2 == 0, code=3)) for i in range(4095 if release else 4096)]
        events += [s.event(120_000_000, s.button(a, False, code=3))] if release else [s.event(0, s.button(a, True, code=4))]
        self.program(a, events)
        if release:
            eventually(self.queue, lambda q: q["pending_events"] == 4095, 30)
            self.input("button", user=b, button="forward", pressed=True)
            eventually(self.queue, lambda q: q["pending_events"] == 4096)
            self.client.tool("routing_input", {"action": "button", "user": b, "button": "forward", "pressed": False}, error=True)
        q = eventually(self.queue, lambda q: q["suspended"] and q["pending_events"] == 0 and q["pending_bytes"] == 0, 20)
        assert q["error"] and q["cancelled"] >= q_before["cancelled"] + 4096
        assert self.state()["running"] and 116 in self.report(d)["held_keys"]
        if release:
            eventually(self.reports, lambda rs: not rs[b]["buttons"] and rs[a]["buttons"] & 8 and rs[b]["mouse_event_count"] == before[b]["mouse_event_count"] + 2)
            self.client.tool("macro_studio", {"operation": "cancel", "user": a})
        else:
            eventually(self.reports, lambda rs: rs[a]["mouse_event_count"] == before[a]["mouse_event_count"] + 4096 and not rs[a]["buttons"], 15)
            assert self.report(b)["mouse_event_count"] == before[b]["mouse_event_count"]
        # Suspended replication lets further ordinary source input through.
        rs = self.reports()
        self.input("button", user=a, button="forward", pressed=True)
        self.input("button", user=a, button="forward", pressed=False)
        eventually(self.reports, lambda value: value[a]["mouse_event_count"] == rs[a]["mouse_event_count"] + 2)
        assert self.report(b if not release else d)["mouse_event_count"] == rs[b if not release else d]["mouse_event_count"]
        self.snapshot("overflow-release-cleanup-keeps-other-source-program-and-native-holds" if release else "actual-cap-overflow-rejects-excess-source-down-suspends-replication-keeps-routing")
        self.apply(_graph.definition(edges))
        assert not self.queue()["suspended"] and not self.queue()["error"]
        self.released()

    def lifecycle(self, eof=False):
        self.reanchor()
        a, b, _, _ = self.users()
        self.set_single({"click_delay_ms": 800})
        self.key(b, "f5", True)
        self.input("button", user=a, button="back", pressed=True)
        eventually(self.queue, lambda q: q["pending_events"] == 1)
        before = self.reports()
        overlays = [u["overlay"]["hwnd"] for u in self.state()["users"]]
        if eof:
            self.client.close()
        else:
            self.input("pause")
            assert not self.queue()["pending_events"]
            self.released()
            self.input("resume")
            self.input("button", user=a, button="back", pressed=True)
            eventually(self.queue, lambda q: q["pending_events"] == 1)
            self.client.tool("routing_stop")
        self.released()
        eventually(lambda: [bool(_graph.user32.IsWindow(h)) for h in overlays], lambda live: not any(live))
        time.sleep(1.0)
        assert self.report(b)["mouse_event_count"] == before[b]["mouse_event_count"]
        self.evidence["checks"]["pending-queue-eof-native-release-overlays-no-late-delivery" if eof else "pending-queue-pause-stop-native-release-overlays-no-late-delivery"] = {"probes": self.reports()}
        self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--ready", action="store_true", help="Producer has confirmed the exclusive frozen live slot")
    parser.add_argument("--queue-tail", action="store_true", help="Preserve prior accepted native prefix; run only its remaining tail")
    parser.add_argument("--overflow-release-tail", action="store_true", help="Preserve queue/ownership prefix; finish cleanup-UP overflow and lifecycle")
    args = parser.parse_args()
    assert args.ready, "No live launch before producer READY"
    for phase in ("native", "permuted", "eof"):
        gate = AdvancedGate(args, permuted=phase == "permuted")
        try:
            gate.start()
            if phase == "native":
                if not args.queue_tail and not args.overflow_release_tail:
                    gate.default_compatibility()
                    gate.component_filters()
                    gate.symmetric_buttons()
                    gate.mixed_coordinates()
                    gate.canonical()
                    gate.pacing()
                if not args.overflow_release_tail:
                    gate.queue_atomicity()
                    gate.origin_cancel()
                    gate.scoped_program_queue_cancel()
                    gate.overflow()
                gate.overflow(release=True)
                gate.lifecycle()
            elif phase == "permuted":
                gate.canonical(reverse=True)
            else:
                gate.lifecycle(eof=True)
            gate.evidence["result"] = "PASS"
            print(f"PASS {phase}: {len(gate.evidence['checks'])} grouped checks. Evidence: {gate.run}", flush=True)
        except Exception as error:
            gate.evidence.update(result="NOT ACCEPTED", failure=repr(error))
            try:
                gate.evidence["failure_observation"] = {"state": gate.state(), "probes": gate.reports()}
            except Exception as observation_error:
                gate.evidence["failure_observation_error"] = repr(observation_error)
            print(f"NOT ACCEPTED {phase}: {error!a}. Evidence: {gate.run}", flush=True)
            raise
        finally:
            gate.close()


if __name__ == "__main__":
    main()
