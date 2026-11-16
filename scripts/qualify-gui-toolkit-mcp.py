"""Separate bounded toolkit MCP lease fixture AFTER the native/Web pair exits normally.

Disposable GitHub-hosted Windows only. Reuses Pair's owned GUI telemetry, private
issuance, stdio RPC and normal-close utilities; no browser/native lifecycle rerun,
new transport, physical input, request replay or process termination.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import threading
import time

spec = importlib.util.spec_from_file_location("pim_toolkit_pair_base", Path(__file__).with_name("qualify-gui-pair.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
require = base.require


class ToolkitPair(base.Pair):
    def __init__(self, args):
        super().__init__(args)
        self.end = self.start + 90
        self.result = {"schema": "pim.gui-toolkit-mcp.v1", "status": "FAIL",
            "source_commit": args.commit, "scope": "one separate GUI observation-only toolkit MCP lease",
            "full_sdk_complete": False, "P1_qualified": False, "release_qualified": False,
            "hardware_qualified": False}

    def toolkit(self, operation):
        reply = self.rpc("tools/call", {"name": "local_user_toolkit", "arguments": {
            "request_json": json.dumps({"version": 1, "operation": operation}, separators=(",", ":"))}})
        require(not reply.get("isError") and len(reply["content"]) == 1)
        require(reply["content"][0]["type"] == "text")
        return json.loads(reply["content"][0]["text"])["result"]

    def toolkit_baseline(self):
        err = open(self.private / "toolkit-mcp-stderr.log", "xb")
        self.mcp = subprocess.Popen([str(self.exe), "mcp", "--toolkit-event-connection", str(self.native_descriptor)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, encoding="utf-8",
            creationflags=subprocess.CREATE_NO_WINDOW)
        err.close()
        self.mcp_queue = queue.Queue()
        def reader():
            try:
                for line in self.mcp.stdout:
                    require(len(line) <= 4 * 1024 * 1024)
                    self.mcp_queue.put(json.loads(line))
            finally:
                self.mcp_queue.put(None)
        threading.Thread(target=reader, daemon=True).start()
        self.rpc_id = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "pim-toolkit-lease-fixture", "version": "1"}})
        self.mcp.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.mcp.stdin.flush()
        tools = self.rpc("tools/list", {})
        require(sorted(tool["name"] for tool in tools["tools"]) == ["local_user_toolkit", "refresh_user_toolkit"])
        self.wait(lambda: self.event_status("active", 2, 1, 1))
        contexts = self.toolkit({"kind": "list"})["users"]
        require(len(contexts) == 2)
        users = [context["user"] for context in contexts]
        ids = [user["id"] for user in users]
        require(len({(user["epoch"], user["local"]) for user in ids}) == 2)
        require(all(user["epoch"] == self.epoch for user in ids))
        require(all(context["app_data"] is None for context in contexts))
        for user in users:
            require(self.toolkit({"kind": "user", "user": user["id"]})["user"]["user"] == user)
            require(self.toolkit({"kind": "source", "source": user["source"]})["user"]["user"] == user)
        self.toolkit({"kind": "set_data", "user": ids[0], "data": {"tag": "toolkit-consumer-local"}})
        require(self.toolkit({"kind": "user", "user": ids[0]})["user"]["app_data"] == {"tag": "toolkit-consumer-local"})
        require(self.toolkit({"kind": "user", "user": ids[1]})["user"]["app_data"] is None)
        colors = [[17, 52, 83, 255], [91, 73, 28, 255]]
        self.toolkit({"kind": "set_palette", "colors": colors})
        require(self.toolkit({"kind": "palette"})["colors"] == colors)
        require(all(context["badge_rgba"] in colors for context in self.toolkit({"kind": "list"})["users"]))
        self.toolkit({"kind": "clear_data", "user": ids[0]})
        require(self.toolkit({"kind": "user", "user": ids[0]})["user"]["app_data"] is None)
        # Keep a verified nonempty annotation cached across the actual GUI revoke.
        self.toolkit({"kind": "set_data", "user": ids[0], "data": {"tag": "toolkit-consumer-local"}})
        require(self.toolkit({"kind": "user", "user": ids[0]})["user"]["app_data"] == {"tag": "toolkit-consumer-local"})
        self.result.update(exact_two_tools=True, selected_user_and_source_lookup=True,
            consumer_local_data_and_palette=True)

    def sample(self, step):
        if step == 5:
            return super().sample(step)
        if step == 6:
            status = self.event_status("local", 0, 0)
            return status and not status["pointer_export"] and not status["keyboard_export"] and not self.descriptors()
        if step == 8:
            return self.event_status("local", 2, 0)
        if step in (11, 16):
            status = self.event_status("local", 2, 0 if step == 11 else 1)
            return status and status["native_observed_users"] == 2 and status["pointer_export"] and status["keyboard_export"] and not self.descriptors()
        if step == 17:
            return self.event_status("active", 2, 1, 0) and not self.descriptors()
        if step == 18:
            return self.issue(1)
        if step == 19:
            return self.event_status("active", 2, 0, 0)
        if step == 20:
            status = self.event_status("off", 0, 0, 0)
            return status and status["retired"] and not status["session_active"] and not self.descriptors()
        raise base.Rejected()

    def toolkit_revoked(self):
        reply = self.rpc("tools/call", {"name": "local_user_toolkit", "arguments": {
            "request_json": '{"version":1,"operation":{"kind":"list"}}'}})
        require(reply.get("isError") is True and "structuredContent" not in reply)
        require(all("toolkit-consumer-local" not in item.get("text", "") for item in reply.get("content", [])))
        refresh = self.rpc("tools/call", {"name": "refresh_user_toolkit", "arguments": {}})
        require(refresh.get("isError") is True)
        self.mcp.stdin.close()
        require(self.mcp.wait(timeout=8) == 0)
        self.result.update(actual_gui_revocation_denies_local_access=True,
            terminal_refresh_denied=True, mcp_exit_code=0)

    def run(self):
        args = self.args
        require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
        require(re.fullmatch("[0-9a-f]{40}", args.commit) is not None)
        prior = self.strict_json("artifact_json", args.pair_result)
        require(prior["status"] == "PASS" and prior["source_commit"] == args.commit)
        require(prior["native_exit_code"] == prior["gui_exit_code"] == 0 and prior["owned_descriptor_cleanup"] is True)
        self.private = args.run.resolve()
        require(not self.private.exists())
        self.private.mkdir()
        self.ui, self.temp = self.private / "ui", self.private / "setup-temp"
        self.ui.mkdir()
        self.temp.mkdir()
        prefs = self.private / "preferences"
        prefs.mkdir()
        self.preferences = prefs / "app-preferences.json"
        shutil.copyfile(args.source / "fixtures/gui-sdk-preparation/app-preferences.json", self.preferences)
        require(self.strict_json("preferences_json", self.preferences)["close_to_tray"] is False)
        fixture = args.source / "fixtures/gui-toolkit-mcp/prepare.json"
        self.steps = self.strict_json("prefix_json", fixture)
        require(len(self.steps) == 20)
        shutil.copyfile(fixture, self.ui / "prepare.json")
        gui_root = args.artifacts / "sdk-preview/gui"
        metadata = self.strict_json("artifact_json", gui_root / "metadata.json")
        require(metadata["source_commit"] == args.commit and metadata["features"] == ["software-renderer"])
        require(metadata["default_features"] is True and metadata["target"] == "x86_64-pc-windows-msvc")
        self.exe = self.checked(gui_root, metadata["executable"], "protoinputmultiplex.exe")
        self.result["gui_binary_sha256"] = metadata["executable"]["sha256"]
        self.stage = "toolkit-gui-preparation"
        env = dict(os.environ, TEMP=str(self.temp), TMP=str(self.temp))
        self.gui = self.launch([self.exe, "gui", "--renderer", "software-dx12", "--preferences", self.preferences,
            "--ui-script", self.ui / "prepare.json", "--ui-script-no-screenshots"], "gui", env)
        self.issued = set()
        for index, step in enumerate(self.steps, 1):
            self.checkpoint = index
            self.check = "toolkit_gui_receipt"
            self.wait(lambda: len(self.receipts()) >= index)
            rows = self.receipts()
            require(len(rows) == index or not step.get("wait_for_sample"))
            require(rows[index - 1]["button"] == step["button"] and rows[index - 1]["clicked"] is True)
            if step.get("wait_for_sample"):
                self.wait(lambda: self.sample(index))
                # Only observations run inside wait; each MCP action executes once.
                if index == 18:
                    self.toolkit_baseline()
                elif index == 19:
                    self.toolkit_revoked()
                self.acknowledge(index)
        self.stage = "toolkit-normal-cleanup"
        self.close_gui_owned_main()
        require(self.gui.wait(timeout=15) == 0)
        require(not self.descriptors() and not list(self.temp.glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json")))
        self.result.update(status="PASS", gui_exit_code=0, zero_authority_cleanup=True,
            prior_pair_closed_normally=True)


def main():
    parser = argparse.ArgumentParser()
    for name in ("artifacts", "source", "run", "public-result", "pair-result"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    pair = ToolkitPair(args)
    try:
        pair.run()
    except BaseException:
        pair.result.update(failure_stage=pair.stage, checkpoint=pair.checkpoint,
            failure_policy="preserve private evidence; no kill/restart/retry")
    pair.result["elapsed_ms"] = int((time.monotonic() - pair.start) * 1000)
    require(not args.public_result.exists())
    args.public_result.write_text(json.dumps(pair.result, indent=2), encoding="utf-8")
    return 0 if pair.result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
