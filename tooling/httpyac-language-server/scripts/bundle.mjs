import { build } from "esbuild";
import { createHash } from "node:crypto";
import { readFile, writeFile, copyFile } from "node:fs/promises";

const result = await build({
  entryPoints: ["src/server.ts"],
  outfile: "bundle/server.cjs",
  bundle: true,
  platform: "node",
  target: "node18",
  format: "cjs",
  // Escape literal newlines so generated strings have no significant trailing whitespace.
  supported: { "template-literal": false },
  legalComments: "eof",
  metafile: true,
});
if (Object.keys(result.metafile.inputs).some((name) => name.includes("node_modules/httpyac/"))) {
  throw new Error("The language server must not bundle the httpyac execution engine");
}
const contents = await readFile("bundle/server.cjs");
await writeFile("bundle/server.sha256", createHash("sha256").update(contents).digest("hex") + "\n");
for (const [source, destination] of [
  ["node_modules/vscode-languageserver/License.txt", "bundle/LICENSE-vscode-languageserver"],
  ["node_modules/vscode-languageserver-textdocument/License.txt", "bundle/LICENSE-vscode-languageserver-textdocument"],
  ["node_modules/vscode-jsonrpc/License.txt", "bundle/LICENSE-vscode-jsonrpc"],
  ["node_modules/vscode-languageserver-protocol/License.txt", "bundle/LICENSE-vscode-languageserver-protocol"],
  ["node_modules/vscode-languageserver-types/License.txt", "bundle/LICENSE-vscode-languageserver-types"],
  ["node_modules/httpyac/LICENSE", "bundle/LICENSE-httpyac-catalog"],
]) {
  await copyFile(source, destination);
}
