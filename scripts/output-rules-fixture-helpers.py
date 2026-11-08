"""Independent output-rule gate preparation; importing never launches fixtures.

Only actual messages recorded by owned automation probes are counted. Runtime
state can explain a result, but cannot substitute for native edge delivery.
"""
import copy


MATRIX = {
    "delivery": "Owned x64/x86 key conversion/remap, default legacy mouse output and disabled sources.",
    "ownership": "Two source aliases plus an unchanged key owner share one native VK: first DOWN/last UP, scoped owner cleanup.",
    "live_change": "Accepted conversion-only change retires affected held source pairs; unrelated holds and active playback survive; a later old-source UP cannot release the new output code.",
    "origins": "Bounded Manual/Studio/Composite/Agent/Replica ownership samples, raw recording and destination-local replication mapping/filter.",
    "atomic_rejection": "Invalid rules, keyboardless and physical seat restrictions preserve accepted rules, held input and program state.",
    "controls": "Actual named Output Rules controls, acknowledged live application, defaults and stopped profile roundtrip.",
    "agent_scope": "Default-denied output configuration, explicit GUI grant on users/PIDs, actual native keyboard-recipient enforcement and scoped disconnect.",
}


def native_key_edges(report, vk, after_us=-1):
    """Return delivered DOWN/UP edges for one VK, independent of sampled holds."""
    return [(e["at_us"], e["pressed"]) for e in report["native_events"]
            if e["kind"] == "key" and e["code"] == vk and e["at_us"] > after_us]


def native_button_edges(report, button, after_us=-1):
    return [(e["at_us"], e["pressed"]) for e in report["native_events"]
            if e["kind"] == "button" and e["code"] == button and e["at_us"] > after_us]


class NativeBaseline:
    """Finite trace boundary; never silently accept the probe's 4096-event cap."""
    def __init__(self, reports):
        self.reports = copy.deepcopy(reports)
        for report in reports:
            assert report["native_event_count"] == len(report["native_events"]), "Native trace truncated"
        self.counts = [len(r["native_events"]) for r in reports]

    def new_events(self, reports, user):
        report = reports[user]
        assert report["native_event_count"] == len(report["native_events"]), "Native trace truncated"
        before = self.reports[user]["native_events"]
        assert report["native_events"][:self.counts[user]] == before, "Probe trace prefix changed"
        return report["native_events"][self.counts[user]:]

    def expect_keys(self, reports, user, expected):
        actual = [(e["code"], e["pressed"]) for e in self.new_events(reports, user) if e["kind"] == "key"]
        assert actual == expected, ("Actual native key edges", actual, expected)

    def expect_no_button_delivery(self, reports, user):
        assert not [e for e in self.new_events(reports, user) if e["kind"] == "button"], "Converted source leaked a native button edge"

    def unchanged_other_target(self, reports, user):
        assert not self.new_events(reports, user), "Input leaked into another owned target"

    def unchanged_holds(self, reports, users):
        for user in users:
            for field in ("held_keys", "buttons"):
                assert reports[user][field] == self.reports[user][field], ("Unrelated hold changed", user, field)
