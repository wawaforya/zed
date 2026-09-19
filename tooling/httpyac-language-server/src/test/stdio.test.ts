import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

interface RpcMessage {
  id?: number;
  method?: string;
  result?: unknown;
  params?: unknown;
  error?: unknown;
}

test("serves initialize, diagnostics, and dynamic completion over stdio", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "httpyac-lsp-stdio-"));
  const scriptMarker = path.join(directory, "script-executed");
  const pluginMarker = path.join(directory, "plugin-loaded");
  const pluginPath = path.join(directory, "malicious-plugin.cjs");
  await fs.writeFile(
    pluginPath,
    `require("node:fs").writeFileSync(${JSON.stringify(pluginMarker)}, "loaded");`,
    "utf8",
  );
  let networkRequests = 0;
  const networkServer = http.createServer((_request, response) => {
    networkRequests++;
    response.end("unexpected");
  });
  await new Promise<void>((resolve) => networkServer.listen(0, "127.0.0.1", resolve));
  const address = networkServer.address();
  assert(address && typeof address === "object");
  const networkUrl = `http://127.0.0.1:${address.port}/must-not-run`;
  const documentText = [
    "{{",
    `require("node:fs").writeFileSync(${JSON.stringify(scriptMarker)}, "executed");`,
    `await fetch(${JSON.stringify(networkUrl)});`,
    "}}",
    "# @name login",
    "POST https://example.invalid/login",
    "###",
    "# @ref ",
    "GET https://example.invalid/me",
  ].join("\n");
  const documentPath = path.join(directory, "e2e.http");
  await fs.writeFile(documentPath, documentText, "utf8");

  const bundledServer = path.join(directory, "server.cjs");
  await fs.copyFile(path.resolve(__dirname, "../../bundle/server.cjs"), bundledServer);
  const server = spawn(process.execPath, [bundledServer, "--stdio"], {
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, HTTPYAC_PLUGIN: pluginPath },
  });
  let output = Buffer.alloc(0);
  const responses = new Map<number, (message: RpcMessage) => void>();
  const notifications: RpcMessage[] = [];
  let wakeNotification: (() => void) | undefined;

  server.stdout.on("data", (chunk: Buffer) => {
    output = Buffer.concat([output, chunk]);
    while (true) {
      const separator = output.indexOf("\r\n\r\n");
      if (separator < 0) break;
      const header = output.subarray(0, separator).toString("ascii");
      const length = Number(header.match(/Content-Length:\s*(\d+)/i)?.[1]);
      if (output.length < separator + 4 + length) break;
      const end = separator + 4 + length;
      const message = JSON.parse(output.subarray(separator + 4, end).toString("utf8")) as RpcMessage;
      output = output.subarray(end);
      if (message.id !== undefined && responses.has(message.id)) {
        responses.get(message.id)?.(message);
        responses.delete(message.id);
      } else if (message.method) {
        notifications.push(message);
        wakeNotification?.();
        wakeNotification = undefined;
      }
    }
  });

  const send = (message: RpcMessage): void => {
    const body = JSON.stringify({ jsonrpc: "2.0", ...message });
    server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
  };
  const request = (id: number, method: string, params: unknown): Promise<RpcMessage> => {
    const response = new Promise<RpcMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 5_000);
      responses.set(id, (message) => {
        clearTimeout(timer);
        resolve(message);
      });
    });
    send({ id, method, params });
    return response;
  };
  const waitForNotification = async (method: string): Promise<RpcMessage> => {
    const deadline = Date.now() + 5_000;
    while (Date.now() < deadline) {
      const found = notifications.find((message) => message.method === method);
      if (found) return found;
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, 100);
        wakeNotification = () => {
          clearTimeout(timer);
          resolve();
        };
      });
    }
    throw new Error(`${method} notification timed out`);
  };

  try {
    const initialized = await request(1, "initialize", {
      processId: process.pid,
      rootUri: pathToFileURL(directory).toString(),
      capabilities: {},
    });
    const initializeResult = initialized.result as { capabilities: Record<string, unknown> };
    assert.equal(initializeResult.capabilities.definitionProvider, true);
    assert.equal(initializeResult.capabilities.renameProvider instanceof Object, true);
    send({ method: "initialized", params: {} });

    const uri = pathToFileURL(documentPath).toString();
    send({
      method: "textDocument/didOpen",
      params: {
        textDocument: {
          uri,
          languageId: "http",
          version: 1,
          text: documentText,
        },
      },
    });

    const completion = await request(2, "textDocument/completion", {
      textDocument: { uri },
      position: { line: 7, character: 7 },
    });
    const completionItems = completion.result as Array<{ label: string }>;
    assert(completionItems.some((item) => item.label === "login"));

    const diagnostic = await waitForNotification("textDocument/publishDiagnostics");
    const published = (diagnostic.params as {
      diagnostics: Array<{ code?: string }>;
    }).diagnostics;
    assert(
      published.some((item) => item.code === "missing-metadata-value"),
      "incomplete @ref should publish a static diagnostic",
    );
    await new Promise((resolve) => setTimeout(resolve, 200));
    assert.equal(await fileExists(scriptMarker), false, "native scripts must never execute");
    assert.equal(await fileExists(pluginMarker), false, "HTTPYAC_PLUGIN must never load");
    assert.equal(networkRequests, 0, "document analysis must never send a request");

    await request(3, "shutdown", null);
    send({ method: "exit", params: null });
  } finally {
    server.kill();
    await new Promise<void>((resolve) => networkServer.close(() => resolve()));
    await fs.rm(directory, { recursive: true, force: true });
  }
});

async function fileExists(fileName: string): Promise<boolean> {
  try {
    await fs.access(fileName);
    return true;
  } catch {
    return false;
  }
}
