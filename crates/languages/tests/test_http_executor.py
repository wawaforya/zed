"""Persistent execution tests, using only a loopback HTTP fixture.

Install httpyac 6.16.7 into a disposable directory, then run:
python crates/languages/tests/test_http_executor.py /path/to/node_modules/httpyac
"""
import concurrent.futures
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ENGINE = str(pathlib.Path(sys.argv[1]).resolve()) if len(sys.argv) > 1 else ""
BRIDGE = pathlib.Path(__file__).resolve().parents[1] / "src" / "http_executor.cjs"


class ExecutionTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zed-httpyac-test-")
        self.base = pathlib.Path(self.temporary.name)
        self.project = self.base / "项目 space ' $ &"
        self.project.mkdir()
        self.session = self.base / "session"
        self.session.mkdir(mode=0o700)
        self.script = self.session / "executor.cjs"
        shutil.copyfile(BRIDGE, self.script)
        self.counts = {"login": 0, "protected": 0, "slow": 0, "create": 0}
        self.authorization = []
        self.posted_bodies = []
        self.response_body = None
        self.response_type = "application/json"
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                kind = self.path.strip("/").split("?")[0]
                with fixture.lock:
                    fixture.counts[kind] += 1
                    count = fixture.counts[kind]
                    if kind == "protected":
                        fixture.authorization.append(self.headers.get("Authorization"))
                if kind == "slow":
                    time.sleep(3)
                payload = json.dumps({"Data": {"Token": f"token-{count}"}, "count": count}).encode()
                self.send_response(200)
                self.send_header("Content-Type", fixture.response_type)
                self.send_header("Set-Cookie", "first=one")
                self.send_header("Set-Cookie", "second=two")
                try:
                    self.end_headers()
                    self.wfile.write(fixture.response_body if fixture.response_body is not None else payload)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                with fixture.lock:
                    fixture.posted_bodies.append(json.loads(body))
                self.do_GET()

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.file = self.project / "请求 space ' $ &.http"
        self.text = "\n".join([
            "# @name ReqA", f"GET {self.url}/login", "###",
            "# @name ReqB", "# @ref ReqA", f"GET {self.url}/protected",
            "Authorization: Bearer {{ReqA.Data.Token}}", "###",
            "# @name Forced", "# @forceRef ReqA", f"GET {self.url}/protected",
            "Authorization: Bearer {{ReqA.Data.Token}}", "###",
            "# @name Slow", f"GET {self.url}/slow", "###",
            "# @name Create", f"GET {self.url}/create", "",
        ])
        self.file.write_text(self.text, encoding="utf-8")
        self.environment = {
            **os.environ, "ZED_HTTPYAC_MODULE": ENGINE,
            "ZED_CUSTOM_HTTPYAC_EXECUTOR": str(self.script),
            "ZED_CUSTOM_HTTPYAC_OWNER": str(os.getpid()),
            "ZED_WORKTREE_ROOT": str(self.project), "ZED_FILE": str(self.file),
        }
        self.environment.pop("HTTPYAC_PLUGIN", None)
        self.environment.pop("ZED_HTTPYAC_ENV", None)

    def tearDown(self):
        try:
            self.run_task(operation="reset")
        finally:
            self.server.shutdown()
            self.server.server_close()
            # Wait for detached workers to release their files on Windows.
            time.sleep(0.2)
            self.temporary.cleanup()

    def spawn(self, line=6, operation="send", output="body", extra_env=None, extra_args=None, shell=None, shell_initialization=None, interactive=False):
        environment = {**self.environment, "ZED_ROW": str(line), **(extra_env or {})}
        arguments = ["node", "-e", "require(process.env.ZED_CUSTOM_HTTPYAC_EXECUTOR)", "--", operation]
        if operation == "send":
            arguments += ["--output", output]
        arguments += extra_args or []
        if shell:
            # Match the unquoted shell command assembled from the built-in task template.
            command = 'node -e "require(process.env.ZED_CUSTOM_HTTPYAC_EXECUTOR)" -- ' + " ".join(arguments[4:])
            if shell_initialization:
                command = shell_initialization + "; " + command
            if shell == "cmd":
                arguments = '"' + os.environ.get("COMSPEC", "cmd.exe") + '" /d /s /c "' + command + '"'
            elif shell in ("pwsh", "powershell"):
                arguments = [shell, "-NoProfile", "-Command", command]
            else:
                arguments = [shell, "-c", command]
        # Legacy Windows PowerShell can leak its redirected pipe handles into detached
        # grandchildren even with stdio=ignore. Capture shell output in files, so waiting
        # for pipe EOF does not falsely wait for the entire worker lifetime.
        files = [open(self.base / ("shell-" + name), "w+b") for name in ("stdout", "stderr")] if shell else None
        process = subprocess.Popen(arguments, cwd=self.project, env=environment, stdin=subprocess.PIPE if interactive else subprocess.DEVNULL,
                                   stdout=files[0] if files else subprocess.PIPE,
                                   stderr=files[1] if files else subprocess.PIPE, text=True, encoding="utf-8")
        process.captured_files = files
        return process

    def run_task(self, expected=0, **options):
        process = self.spawn(**options)
        try:
            stdout, stderr = process.communicate(timeout=30)
            if process.captured_files:
                for file in process.captured_files:
                    file.seek(0)
                stdout, stderr = [file.read().decode("utf-8", errors="replace") for file in process.captured_files]
            self.assertEqual(process.returncode, expected, stdout + stderr)
            return stdout, stderr
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
            raise
        finally:
            for file in process.captured_files or []:
                file.close()

    def records(self):
        return [json.loads(file.read_text()) for file in self.session.glob("*/endpoint.json")]

    def machine_task(self, expected=0, answer=None, **options):
        process = self.spawn(interactive=True, extra_args=["--json", *options.pop("extra_args", [])], **options)
        events = []
        try:
            for line in process.stdout:
                self.assertLessEqual(len(line.encode()), 2 * 1024 * 1024)
                event = json.loads(line)
                events.append(event)
                if event["type"] == "prompt":
                    self.assertIsNotNone(answer)
                    process.stdin.write(json.dumps({"answer": event["id"], "value": answer}) + "\n")
                    process.stdin.flush()
            process.wait(timeout=5)
            stderr = process.stderr.read()
            self.assertEqual(process.returncode, expected, stderr + str(events))
            self.assertEqual(events[-1], {"type": "done", "code": expected})
            return events
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()

    def test_structured_responses_reuse_sessions_and_preserve_headers(self):
        import base64
        events = self.machine_task()
        responses = [event for event in events if event["type"] == "response"]
        self.assertEqual(len(responses), 2)
        response = responses[-1]
        self.assertEqual(response["status"], 200)
        self.assertIn("token-1", response["body"])
        self.assertIn("first=one", response["headers"])
        self.assertIn("second=two", response["headers"])
        self.assertIn("token-1", response["request"])
        self.assertIn("total", response["timings"])
        self.assertEqual(len(base64.b64decode(response["rawBody"])), response["bodyBytes"])
        self.machine_task()
        self.run_task()
        self.assertEqual(self.counts["login"], 1)
        for file in self.session.rglob("*"):
            if file.is_file() and file.name != "executor.cjs":
                self.assertNotIn("token-1", file.read_text())

    def test_structured_start_detects_explicit_stream_protocols(self):
        for request in [
            "WS ws://127.0.0.1:1/events", "WSS wss://127.0.0.1:1/events",
            "WEBSOCKET ws://127.0.0.1:1/events", "ws://127.0.0.1:1/events",
            "wss://127.0.0.1:1/events", f"SSE {self.url}/create",
            f"EVENTSOURCE {self.url}/create",
        ]:
            with self.subTest(request=request):
                # Exercise the real parser without opening a long-lived connection.
                self.file.write_text(f"# @disabled\n{request}\n###\nGET {self.url}/create\n", encoding="utf-8")
                events = self.machine_task(line=2)
                self.assertEqual(events[0], {"type": "started", "streaming": True})
                self.assertEqual(sum(event["type"] == "started" for event in events), 1)
                events = self.machine_task(line=4)
                self.assertEqual(events[0], {"type": "started", "streaming": False})
        events = self.machine_task(extra_args=["--all"])
        self.assertEqual(events[0], {"type": "started", "streaming": True})

    def test_ordinary_http_event_stream_does_not_select_stream_view(self):
        self.response_type = "text/event-stream"
        self.response_body = b'data: {"message":"hello"}\n\n'
        for request in [f"GET {self.url}/create", f'POST {self.url}/create\nContent-Type: application/json\n\n{{}}']:
            with self.subTest(request=request):
                self.file.write_text(request + "\n", encoding="utf-8")
                events = self.machine_task(line=1)
                self.assertEqual(events[0], {"type": "started", "streaming": False})
                self.assertTrue(any(event["type"] == "response" for event in events))

    def test_structured_script_output_and_assertions_are_separate(self):
        self.file.write_text(f'''GET {self.url}/create
> {{%
console.log('not JSON: {{"type":"done"}}');
client.test("intentional failure", function() {{ client.assert(false, "failed assertion"); }});
%}}
''', encoding="utf-8")
        events = self.machine_task(line=1, expected=1)
        self.assertTrue(any(event["type"] == "output" and "not JSON" in event["text"] for event in events))
        self.assertTrue(any(event["type"] == "test" and event["status"] in ("FAILED", "ERROR") for event in events))
        self.assertEqual(sum(event["type"] == "done" for event in events), 1)
        self.assertEqual(self.counts["create"], 1)

    def test_structured_large_and_binary_responses_are_bounded(self):
        import base64
        self.response_body = bytes(range(256)) * 4096
        self.response_type = "application/octet-stream"
        events = self.machine_task(line=2)
        response = next(event for event in events if event["type"] == "response")
        self.assertTrue(response["truncated"])
        self.assertEqual(response["bodyBytes"], len(self.response_body))
        self.assertEqual(base64.b64decode(response["rawBody"]), self.response_body[:512 * 1024])

    def test_structured_prompt_uses_stdin_without_a_terminal(self):
        self.file.write_text(f'GET {self.url}/protected\nAuthorization: {{{{$password secret}}}}\n', encoding="utf-8")
        events = self.machine_task(line=1, answer="test-secret")
        self.assertTrue(any(event["type"] == "prompt" for event in events))
        self.assertEqual(self.authorization, ["test-secret"])

    def test_structured_send_all_and_reset(self):
        events = self.machine_task(extra_args=["--all"])
        self.assertGreaterEqual(sum(event["type"] == "response" for event in events), 5)
        self.assertEqual(self.counts, {"login": 2, "protected": 2, "slow": 1, "create": 1})
        self.machine_task(operation="reset", extra_env={"ZED_HTTPYAC_MODULE": "missing"})
        self.machine_task()
        self.assertEqual(self.counts["login"], 3)

    def test_structured_disconnect_cancels_without_retry(self):
        process = self.spawn(line=15, interactive=True, extra_args=["--json"])
        deadline = time.monotonic() + 10
        while self.counts["slow"] == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(self.counts["slow"], 1)
        process.stdin.close()
        process.stdin = None
        process.communicate(timeout=5)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(self.counts["slow"], 1)
        self.machine_task()
        self.assertEqual(self.counts["login"], 1)

    def test_previous_login_and_ref_are_reused_and_force_ref_refreshes(self):
        self.run_task(line=2)
        self.run_task()
        self.run_task()
        self.assertEqual(self.counts["login"], 1)
        for file in self.session.glob("*/endpoint.json"):
            self.assertNotIn("token-1", file.read_text())
        self.assertEqual(self.authorization, ["Bearer token-1", "Bearer token-1"])
        self.run_task(line=11)
        self.assertEqual(self.counts["login"], 2)
        self.run_task()
        self.assertEqual(self.authorization[-1], "Bearer token-2")
        self.run_task(line=2)
        self.run_task()
        self.assertEqual(self.counts["login"], 3)
        self.assertEqual(self.authorization[-1], "Bearer token-3")

    def test_first_ref_executes_dependency_once_and_reset_forces_new_session(self):
        self.run_task()
        self.run_task()
        self.assertEqual(self.counts["login"], 1)
        self.run_task(operation="reset")
        self.run_task()
        self.assertEqual(self.counts["login"], 2)

    def test_parallel_first_clicks_share_one_worker_and_serialize_requests(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(lambda _: self.run_task(), range(3)))
        self.assertEqual(self.counts["login"], 1)
        self.assertEqual(self.counts["protected"], 3)
        self.assertEqual(len(self.records()), 1)

    def test_environments_and_projects_are_isolated(self):
        self.run_task(extra_env={"ZED_HTTPYAC_ENV": "dev"})
        self.run_task(extra_env={"ZED_HTTPYAC_ENV": "prod"})
        self.run_task(extra_env={"ZED_HTTPYAC_ENV": "dev"})
        self.assertEqual(self.counts["login"], 2)
        other = self.base / "other"
        other.mkdir()
        target = other / "requests.http"
        target.write_text(self.text, encoding="utf-8")
        self.run_task(extra_env={"ZED_WORKTREE_ROOT": str(other), "ZED_FILE": str(target)})
        self.assertEqual(self.counts["login"], 3)
        self.run_task(operation="reset", extra_env={"ZED_WORKTREE_ROOT": str(other)})

    def test_environment_values_are_loaded(self):
        (self.project / "http-client.env.json").write_text(json.dumps({
            "dev": {"host": self.url, "token": "dev-secret"},
            "prod": {"host": self.url, "token": "prod-secret"},
        }), encoding="utf-8")
        self.file.write_text("GET {{host}}/protected\nAuthorization: {{token}}\n", encoding="utf-8")
        self.run_task(line=1, extra_env={"ZED_HTTPYAC_ENV": "dev"})
        self.run_task(line=1, extra_env={"ZED_HTTPYAC_ENV": '["prod"]'})
        self.run_task(line=1, extra_env={"ZED_HTTPYAC_ENV": "dev"})
        self.assertEqual(self.authorization, ["dev-secret", "prod-secret", "dev-secret"])

    def test_reset_does_not_load_project_code_or_require_httpyac(self):
        marker = self.project / "executed.txt"
        (self.project / ".httpyac.cjs").write_text("require('node:fs').writeFileSync(" + json.dumps(str(marker)) + ", 'executed'); module.exports = {};", encoding="utf-8")
        self.run_task(operation="reset", extra_env={"ZED_HTTPYAC_MODULE": str(self.base / "missing")})
        self.assertFalse(marker.exists())
        self.run_task()
        self.assertTrue(marker.exists())

    def test_worker_crash_discards_cache_and_next_explicit_send_recovers(self):
        self.run_task()
        record = self.records()[0]
        subprocess.run(["node", "-e", "process.kill(Number(process.argv[1]), 'SIGKILL')", str(record["pid"])], check=True)
        time.sleep(0.2)
        self.run_task()
        self.assertEqual(self.counts["login"], 2)
        self.assertEqual(self.counts["protected"], 2)

    def test_editing_post_body_preserves_unchanged_dependency(self):
        prefix = f'''###
# @name ReqA
GET {self.url}/login

###
# @name ReqB
# @ref ReqA
POST {self.url}/protected
Content-Type: application/json

'''
        first = '{"data1": "{{ReqA.Data.Token}}"}\n'
        second = '{"data1": "{{ReqA.Data.Token}}", "data2": "{{ReqA.count}}"}\n'
        self.file.write_text(prefix + first, encoding="utf-8")
        self.run_task(line=3)
        self.run_task(line=8)
        worker = self.records()[0]["pid"]
        for body in (second, first):
            self.file.write_text(prefix + body, encoding="utf-8")
            self.run_task(line=8)
            self.assertEqual(self.counts["login"], 1)
            self.assertEqual(self.records()[0]["pid"], worker)
        self.assertEqual(self.posted_bodies, [
            {"data1": "token-1"}, {"data1": "token-1", "data2": "1"}, {"data1": "token-1"},
        ])
        self.file.write_text(prefix.replace("@ref", "@forceRef") + second, encoding="utf-8")
        self.run_task(line=8)
        self.assertEqual(self.counts["login"], 2)
        self.assertEqual(self.posted_bodies[-1], {"data1": "token-2", "data2": "2"})

    def test_inserted_request_and_header_edits_preserve_dependency(self):
        self.run_task()
        text = f"# @name Inserted\nGET {self.url}/create\n###\n" + self.text
        self.file.write_text(text, encoding="utf-8")
        self.run_task(line=9)
        text = text.replace("/protected", "/protected?changed").replace("Authorization: Bearer", "X-Test: changed\nAuthorization: Bearer")
        self.file.write_text(text, encoding="utf-8")
        self.run_task(line=9)
        self.assertEqual(self.counts["login"], 1)
        self.assertEqual(self.counts["create"], 0)

    def test_dependency_edit_invalidates_transitive_cached_results(self):
        text = self.text[:self.text.index("# @name Forced")] + f"# @name ReqC\n# @ref ReqB\nGET {self.url}/create\n"
        self.file.write_text(text, encoding="utf-8")
        self.run_task(line=11)
        self.run_task(line=11)
        self.assertEqual(self.counts["login"], 1)
        self.assertEqual(self.counts["protected"], 1)
        self.file.write_text(text.replace("/login", "/login?changed"), encoding="utf-8")
        self.run_task(line=11)
        self.assertEqual(self.counts["login"], 2)
        self.assertEqual(self.counts["protected"], 2)
        self.assertEqual(self.authorization[-1], "Bearer token-2")

    def test_global_variable_and_script_edits_invalidate_dependencies(self):
        text = f"@endpoint = {self.url}/login\n###\n" + self.text.replace(f"{self.url}/login", "{{endpoint}}")
        self.file.write_text(text, encoding="utf-8")
        self.run_task(line=8)
        self.file.write_text(text.replace("/login", "/login?changed"), encoding="utf-8")
        self.run_task(line=8)
        self.assertEqual(self.counts["login"], 2)
        text = '{{\nconsole.log("global one");\n}}\n###\n' + self.text
        self.file.write_text(text, encoding="utf-8")
        self.run_task(line=10)
        self.file.write_text(text.replace("global one", "global two"), encoding="utf-8")
        self.run_task(line=10)
        self.assertEqual(self.counts["login"], 4)

    def test_removed_or_renamed_dependency_does_not_resurrect_copied_variables(self):
        self.run_task()
        renamed = self.text.replace("ReqA", "Renamed")
        self.file.write_text(renamed, encoding="utf-8")
        self.run_task()
        self.assertEqual(self.counts["login"], 2)
        self.file.write_text(renamed[renamed.index("# @name ReqB"):], encoding="utf-8")
        self.run_task(line=3, expected=1)
        self.assertEqual(self.counts["login"], 2)
        self.assertEqual(self.counts["protected"], 2)

    def test_file_and_config_changes_invalidate_cached_credentials(self):
        self.run_task()
        self.file.write_text(self.text.replace("/login", "/login?changed"), encoding="utf-8")
        self.run_task()
        self.assertEqual(self.counts["login"], 2)
        (self.project / ".httpyac.json").write_text('{"log":{"level":40}}', encoding="utf-8")
        self.run_task()
        self.assertEqual(self.counts["login"], 3)
        (self.project / ".env").write_text("MY_TEST_TOKEN=new\n", encoding="utf-8")
        self.run_task()
        self.assertEqual(self.counts["login"], 4)

    def test_external_body_change_still_restarts_session(self):
        body = self.project / "payload.json"
        body.write_text('{"value": 1}', encoding="utf-8")
        self.file.write_text(f'''# @name ReqA
GET {self.url}/login
###
# @ref ReqA
POST {self.url}/protected
Content-Type: application/json

< ./payload.json
''', encoding="utf-8")
        self.run_task(line=5)
        worker = self.records()[0]["pid"]
        body.write_text('{"value": 2}', encoding="utf-8")
        self.run_task(line=5)
        self.assertEqual(self.counts["login"], 2)
        self.assertNotEqual(self.records()[0]["pid"], worker)
        self.assertEqual(self.posted_bodies, [{"value": 1}, {"value": 2}])

    def test_imported_request_is_reused_and_import_change_invalidates(self):
        imported = self.project / "auth.http"
        imported.write_text(f"# @name ReqA\nGET {self.url}/login\n###\n# @name Unrelated\nGET {self.url}/create\n", encoding="utf-8")
        self.file.write_text(f"# @import ./auth.http\n# @ref ReqA\nGET {self.url}/protected\nAuthorization: Bearer {{{{ReqA.Data.Token}}}}\n", encoding="utf-8")
        self.run_task(line=3)
        self.run_task(line=3)
        self.assertEqual(self.counts["login"], 1)
        self.file.write_text(self.file.read_text(encoding="utf-8").replace("/protected", "/protected?changed"), encoding="utf-8")
        self.run_task(line=3)
        imported.write_text(imported.read_text(encoding="utf-8").replace("/create", "/create?changed"), encoding="utf-8")
        self.run_task(line=3)
        self.assertEqual(self.counts["login"], 1)
        self.assertEqual(self.counts["create"], 0)
        imported.write_text(f"# @name ReqA\nGET {self.url}/login?new\n", encoding="utf-8")
        self.run_task(line=3)
        self.assertEqual(self.counts["login"], 2)

    def test_output_formats_and_invalid_line_never_send_all(self):
        body, _ = self.run_task(output="body")
        self.assertIn('Token', body)
        headers, _ = self.run_task(output="headers")
        self.assertIn('content-type', headers.lower())
        self.assertNotIn('"Token"', headers)
        exchange, _ = self.run_task(output="exchange")
        self.assertIn('GET', exchange)
        counts = self.counts.copy()
        self.run_task(line=10000, expected=1)
        self.assertEqual(self.counts, counts)

    def test_shell_task_command_and_module_discovery(self):
        launcher = self.base / "bin with spaces"
        launcher.mkdir()
        if os.name == "nt":
            (launcher / "httpyac.cmd").write_text("@echo off\n", encoding="utf-8")
            (launcher / "node_modules").mkdir()
            # A small package wrapper exercises npm's Windows launcher layout.
            package = launcher / "node_modules" / "httpyac"
            package.mkdir()
            (package / "package.json").write_text(json.dumps({"name": "httpyac", "version": "6.16.7", "main": "index.cjs"}))
            (package / "index.cjs").write_text("module.exports = require(" + json.dumps(ENGINE) + ");")
            shells = ["cmd", "powershell"] + (["pwsh"] if shutil.which("pwsh") else [])
        else:
            (launcher / "httpyac").symlink_to(pathlib.Path(ENGINE) / "dist" / "index.js")
            shells = ["sh", "bash"]
        extra = {"ZED_HTTPYAC_MODULE": "", "PATH": str(launcher) + os.pathsep + os.environ.get("PATH", "")}
        for shell in shells:
            with self.subTest(shell=shell):
                self.run_task(shell=shell, extra_env=extra)
                before = self.counts["login"]
                self.run_task(shell=shell, extra_env=extra)
                self.assertEqual(self.counts["login"], before)
                self.assertGreater(before, 0)

    def test_direct_variable_scripts_and_failed_assertions(self):
        self.file.write_text(self.text.replace("# @ref ReqA\n", ""), encoding="utf-8")
        self.run_task(line=2)
        self.run_task(line=5)
        self.run_task(line=5)
        self.assertEqual(self.counts["login"], 1)
        self.file.write_text(f'''GET {self.url}/create
> {{%
console.log("script ran");
client.test("intentional failure", function() {{ client.assert(false, "failed assertion"); }});
%}}
''', encoding="utf-8")
        stdout, stderr = self.run_task(line=1, expected=1)
        self.assertIn("script ran", stdout + stderr)
        self.assertEqual(self.counts["create"], 1)

    def test_send_all_requests(self):
        self.run_task(extra_args=["--all"])
        self.assertEqual(self.counts, {"login": 2, "protected": 2, "slow": 1, "create": 1})

    def test_shell_generated_starship_session_does_not_split_worker(self):
        # Reproduce prompt initialization in each fresh task shell, without loading
        # the user's unrelated profile or requiring Starship to be installed.
        if os.name == "nt":
            shell = "pwsh" if shutil.which("pwsh") else "powershell"
            initialization = "$env:STARSHIP_SESSION_KEY = [guid]::NewGuid().ToString(); Write-Output ('session:' + $env:STARSHIP_SESSION_KEY)"
        else:
            shell = "sh"
            initialization = 'export STARSHIP_SESSION_KEY=$$; printf "session:%s\\n" "$STARSHIP_SESSION_KEY"'
        sessions = []
        for line in (2, 6, 6):
            stdout, _ = self.run_task(line=line, shell=shell, shell_initialization=initialization)
            sessions.append(next(line for line in stdout.splitlines() if line.startswith("session:")))
        self.assertEqual(len(set(sessions)), 3)
        self.assertEqual(self.counts["login"], 1)
        self.assertEqual(self.counts["protected"], 2)
        self.assertEqual(len(self.records()), 1)
        self.run_task(line=11, shell=shell, shell_initialization=initialization)
        self.assertEqual(self.counts["login"], 2)

    def test_terminal_and_ssh_metadata_do_not_split_worker(self):
        names = [
            "STARSHIP_SESSION_KEY", "TERM_SESSION_ID", "WT_SESSION", "WINDOWID",
            "SSH_CLIENT", "SSH_CONNECTION", "SSH_TTY", "TTY", "COLUMNS", "LINES",
        ]
        baseline = {name: "first" for name in names}
        self.run_task(line=2, extra_env=baseline)
        for name in names:
            with self.subTest(variable=name):
                self.run_task(extra_env={**baseline, name: "second"})
                self.assertEqual(self.counts["login"], 1)
                self.assertEqual(len(self.records()), 1)
        self.assertEqual(self.counts["protected"], len(names))

    def test_changed_process_environment_gets_a_new_session(self):
        # Do not ignore entire SSH_/STARSHIP_ namespaces: agent credentials and
        # configuration paths must still partition the execution environment.
        for name in ("HTTP_EXECUTOR_TEST_SECRET", "SSH_AUTH_SOCK", "STARSHIP_CONFIG"):
            with self.subTest(variable=name):
                before = self.counts["login"]
                self.run_task(extra_env={name: "first"})
                self.run_task(extra_env={name: "second"})
                self.run_task(extra_env={name: "first"})
                self.assertEqual(self.counts["login"], before + 2)

    def test_reset_cancels_an_active_request(self):
        process = self.spawn(line=15)
        deadline = time.monotonic() + 10
        while self.counts["slow"] == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(self.counts["slow"], 1)
        self.run_task(operation="reset")
        stdout, stderr = process.communicate(timeout=5)
        self.assertNotEqual(process.returncode, 0, stdout + stderr)
        self.assertEqual(self.counts["slow"], 1)

    def test_owner_exit_stops_worker_and_discards_credentials(self):
        owner = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        try:
            self.run_task(extra_env={"ZED_CUSTOM_HTTPYAC_OWNER": str(owner.pid)})
            self.assertTrue(self.records())
        finally:
            owner.kill()
            owner.wait(timeout=5)
        deadline = time.monotonic() + 5
        while self.records() and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertFalse(self.records())
        self.run_task()
        self.assertEqual(self.counts["login"], 2)

    def test_unauthenticated_client_cannot_send(self):
        self.run_task()
        record = self.records()[0]
        with socket.create_connection(("127.0.0.1", record["port"]), timeout=3) as connection:
            connection.sendall((json.dumps({"token": "wrong", "operation": "send", "file": str(self.file), "line": 18, "output": "body"}) + "\n").encode())
            self.assertEqual(connection.recv(1024), b"")
        self.assertEqual(self.counts["create"], 0)
        self.run_task()
        self.assertEqual(self.counts["login"], 1)

    def test_cancellation_drops_session_without_replaying_request(self):
        self.run_task()
        process = self.spawn(line=15)
        deadline = time.monotonic() + 10
        while self.counts["slow"] == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(self.counts["slow"], 1)
        process.kill()
        process.communicate(timeout=5)
        time.sleep(0.3)
        self.run_task()
        self.assertEqual(self.counts["slow"], 1)
        self.assertEqual(self.counts["login"], 2)


if __name__ == "__main__":
    if not ENGINE:
        raise SystemExit(__doc__)
    unittest.main(argv=[sys.argv[0]], verbosity=2)
