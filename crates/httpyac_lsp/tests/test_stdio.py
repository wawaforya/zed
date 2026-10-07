"""Exercise the embedded Rust server in Zed, remote_server, or the stdio example.

python crates/httpyac_lsp/tests/test_stdio.py path/to/executable
"""
import json
import os
import pathlib
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class ProtocolTest(unittest.TestCase):
    def test_embedded_server(self):
        executable = str(pathlib.Path(sys.argv[1]).resolve())
        with tempfile.TemporaryDirectory(prefix="httpyac-rust-") as temporary:
            directory = pathlib.Path(temporary)
            marker = directory / "executed"
            plugin = directory / "plugin.cjs"
            plugin.write_text(f"require('fs').writeFileSync({json.dumps(str(marker))}, 'plugin');", encoding="utf-8")
            (directory / "httpyac.config.js").write_text(plugin.read_text(encoding="utf-8"), encoding="utf-8")
            network_calls = []

            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    network_calls.append(self.path)
                    self.send_response(200)
                    self.end_headers()

            network = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            threading.Thread(target=network.serve_forever, daemon=True).start()
            shared = directory / "中文 shared.http"
            shared.write_text("# @name My Login\nGET https://example.invalid\n", encoding="utf-8")
            unrelated = directory / "unrelated.http"
            unrelated.write_text("# @name My Login\nGET https://unrelated.invalid\n", encoding="utf-8")
            main = directory / "main.http"
            text = "\r\n".join([
                "{{",
                f"require('fs').writeFileSync({json.dumps(str(marker))}, 'script');",
                "}}",
                '# @import "./中文 shared.http"',
                "# @ref My Login",
                f"GET http://127.0.0.1:{network.server_port}/must-not-run",
                "X-Test: 😀{{MyLogin.token}}",
                "",
            ])
            main.write_text(text, encoding="utf-8")
            process = subprocess.Popen(
                [executable, "--httpyac-language-server"], cwd=directory,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env={**os.environ, "HTTPYAC_PLUGIN": str(plugin)},
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
            )
            messages = queue.Queue()
            errors = []

            def read_output():
                try:
                    while True:
                        header = process.stdout.readline()
                        if not header:
                            return
                        self.assertTrue(header.startswith(b"Content-Length:"), header)
                        length = int(header.split(b":", 1)[1])
                        self.assertEqual(process.stdout.readline(), b"\r\n")
                        messages.put(json.loads(process.stdout.read(length)))
                except BaseException as error:
                    messages.put(error)

            threading.Thread(target=read_output, daemon=True).start()
            threading.Thread(target=lambda: errors.append(process.stderr.read().decode("utf-8", errors="replace")), daemon=True).start()
            sequence = 0
            pending = []

            def send(message):
                body = json.dumps({"jsonrpc": "2.0", **message}, ensure_ascii=False).encode("utf-8")
                process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body)
                process.stdin.flush()

            def receive(predicate, timeout=15):
                deadline = time.monotonic() + timeout
                while True:
                    for index, message in enumerate(pending):
                        if predicate(message):
                            return pending.pop(index)
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise AssertionError(f"LSP timeout, stderr={errors}, pending={pending}")
                    message = messages.get(timeout=remaining)
                    if isinstance(message, BaseException):
                        raise message
                    if message.get("method") in ("client/registerCapability", "client/unregisterCapability"):
                        send({"id": message["id"], "result": None})
                    else:
                        pending.append(message)

            def request(method, params):
                nonlocal sequence
                sequence += 1
                send({"id": sequence, "method": method, "params": params})
                response = receive(lambda message: message.get("id") == sequence)
                self.assertNotIn("error", response, response)
                return response["result"]

            def notify(method, params):
                send({"method": method, "params": params})

            def diagnostics(version=None, code=None, absent=None):
                def matches(message):
                    if message.get("method") != "textDocument/publishDiagnostics":
                        return False
                    params = message["params"]
                    if params["uri"] != main.as_uri() or (version is not None and params.get("version") != version):
                        return False
                    codes = {item["code"] for item in params["diagnostics"]}
                    return (code is None or code in codes) and (absent is None or absent not in codes)
                return receive(matches)["params"]

            try:
                initialized = request("initialize", {"processId": os.getpid(), "rootUri": directory.as_uri(), "capabilities": {"workspace": {"didChangeWatchedFiles": {"dynamicRegistration": True}}}})
                self.assertEqual(initialized["capabilities"]["positionEncoding"], "utf-16")
                notify("initialized", {})
                notify("textDocument/didOpen", {"textDocument": {"uri": main.as_uri(), "version": 1, "languageId": "http", "text": text}})
                diagnostics(version=1, absent="undefined-ref")
                params = {"textDocument": {"uri": main.as_uri()}, "position": {"line": 4, "character": 12}}
                completion = request("textDocument/completion", params)
                self.assertTrue(any(item["label"] == "My Login" for item in completion))
                definition = request("textDocument/definition", params)
                self.assertEqual([item["uri"] for item in definition], [shared.as_uri()])
                references = request("textDocument/references", {**params, "context": {"includeDeclaration": True}})
                self.assertEqual(len(references), 3)
                self.assertNotIn(unrelated.as_uri(), [item["uri"] for item in references])
                rename = request("textDocument/rename", {**params, "newName": "New Session"})
                self.assertEqual(len(rename["documentChanges"]), 2)
                response_edit = next(edit for change in rename["documentChanges"] if change["textDocument"]["uri"] == main.as_uri() for edit in change["edits"] if edit["newText"] == "NewSession")
                self.assertEqual(response_edit["range"]["start"], {"line": 6, "character": 12})
                self.assertEqual(response_edit["range"]["end"], {"line": 6, "character": 19})
                notify("textDocument/didChange", {"textDocument": {"uri": main.as_uri(), "version": 2}, "contentChanges": [{"range": {"start": {"line": 6, "character": 8}, "end": {"line": 6, "character": 10}}, "text": "中"}]})
                diagnostics(version=2)
                shared.write_text("# @name changed\nGET https://example.invalid\n", encoding="utf-8")
                notify("workspace/didChangeWatchedFiles", {"changes": [{"uri": shared.as_uri(), "type": 2}]})
                diagnostics(version=2, code="undefined-ref")
                shared.write_text("# @name My Login\nGET https://example.invalid\n", encoding="utf-8")
                # No client notification: exercise polling fallback and dependency refresh.
                diagnostics(version=2, absent="undefined-ref")
                self.assertFalse(marker.exists())
                self.assertEqual(network_calls, [])
                self.assertIsNone(request("shutdown", None))
                notify("exit", None)
                self.assertEqual(process.wait(timeout=10), 0)
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
                process.stdin.close()
                process.stdout.close()
                process.stderr.close()
                network.shutdown()
                network.server_close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    unittest.main(argv=[sys.argv[0]], verbosity=2)
