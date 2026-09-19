import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import {
  completions,
  catalogVariableNames,
  codeActions,
  definition,
  diagnostics,
  documentLinks,
  formattingEdits,
  hover,
  references,
  rename,
  workspaceSymbols,
} from "../features";
import { loadSafeHttpYacCatalog } from "../security";
import { WorkspaceIndex } from "../workspace";

test("provides imported request completion, diagnostics, navigation, rename, and links", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "httpyac-lsp-"));
  const sharedPath = path.join(directory, "shared.http");
  const mainPath = path.join(directory, "main.http");
  await fs.writeFile(sharedPath, "# @name login\nPOST https://example.invalid/login\n", "utf8");
  const mainText = [
    "baseUrl = https://example.invalid",
    "# @import ./shared.http",
    "# @ref login",
    "GET {{baseUrl}}/me",
    "Authorization: Bearer {{login.token}}",
    "X-Generated: {{$timestamp}}",
  ].join("\n");
  await fs.writeFile(mainPath, mainText, "utf8");

  const workspace = new WorkspaceIndex();
  const index = workspace.update(pathToFileURL(mainPath).toString(), mainText);
  await workspace.loadImports(index);
  const catalog = loadSafeHttpYacCatalog();
  const items = await completions(workspace, index, { line: 2, character: 12 }, catalog);
  assert(items.some((item) => item.label === "login"));

  const foundDiagnostics = await diagnostics(
    workspace,
    index,
    new Set(catalog.metadata.map((item) => item.name.toLowerCase())),
  );
  assert.equal(foundDiagnostics.length, 0);
  assert.equal((await definition(workspace, index, { line: 2, character: 9 })).length, 1);
  assert.equal(references(workspace, index, { line: 2, character: 9 }).length, 3);
  assert.equal((await definition(workspace, index, { line: 4, character: 28 })).length, 1);
  const requestRename = rename(workspace, index, { line: 2, character: 9 }, "authenticate");
  assert(requestRename?.changes?.[index.uri]?.some((edit) =>
    edit.newText === "authenticate" &&
    edit.range.start.line === 4 &&
    edit.range.end.character - edit.range.start.character === "login".length
  ), "request rename must update only the named-response root");
  const variableRename = rename(workspace, index, { line: 3, character: 8 }, "apiBase");
  assert.equal(
    variableRename?.changes?.[index.uri]?.filter((edit) => edit.newText === "apiBase").length,
    2,
    "variable rename must update its definition and reference",
  );
  const builtinHover = hover(index, { line: 5, character: 17 }, catalog);
  assert.match(JSON.stringify(builtinHover), /Timestamp/i);
  const headerHover = hover(index, { line: 4, character: 4 }, catalog);
  assert.match(JSON.stringify(headerHover), /Authentication credentials/i);
  assert.equal(documentLinks(index).length, 1);
  assert(workspaceSymbols(workspace, "log").some((item) => item.name === "login"));
});

test("reports undefined names, missing files, invalid methods, invalid JSON, and cycles", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "httpyac-lsp-errors-"));
  const mainPath = path.join(directory, "main.http");
  const childPath = path.join(directory, "child.http");
  const mainText = [
    "# @import ./child.http",
    "# @import ./missing.http",
    "# @ref absent",
    "# @timeout eventually",
    "# @loop around forever",
    "# @ratelimit max many",
    "FETCH https://example.invalid",
    "Content-Type: application/json",
    "X-Test: {{missingVariable}}",
    "",
    '{"broken":}',
  ].join("\n");
  await fs.writeFile(mainPath, mainText, "utf8");
  await fs.writeFile(childPath, [
    "# @import ./main.http",
    "# @name first",
    "# @ref second",
    "GET https://example.invalid/first",
    "###",
    "# @name second",
    "# @ref first",
    "GET https://example.invalid/second",
    "",
  ].join("\n"), "utf8");
  const workspace = new WorkspaceIndex();
  const index = workspace.update(pathToFileURL(mainPath).toString(), mainText);
  const catalog = loadSafeHttpYacCatalog();
  const found = await diagnostics(
    workspace,
    index,
    new Set(catalog.metadata.map((item) => item.name.toLowerCase())),
    catalogVariableNames(catalog),
  );
  const codes = new Set(found.map((item) => item.code));
  for (const code of [
    "undefined-ref", "undefined-variable", "missing-file", "invalid-method",
    "invalid-json", "import-cycle", "reference-cycle", "invalid-metadata-value",
  ]) {
    assert(codes.has(code), `expected ${code}; received ${[...codes].join(", ")}`);
  }
});

test("accepts httpYac body comments without accepting general JSONC", async () => {
  const catalog = loadSafeHttpYacCatalog();
  const knownMetadata = new Set(catalog.metadata.map((item) => item.name.toLowerCase()));
  const knownVariables = catalogVariableNames(catalog);

  const validText = [
    "POST https://example.invalid/items",
    "Content-Type: application/json",
    "",
    "// comment before the JSON value",
    "{",
    "  // removed before httpYac sends the body",
    '  "first": 1,',
    "  /*",
    "   * removed as well",
    "   */",
    '  "second": 2',
    "}",
    "",
    "> {%",
    'client.global.set("created", response.body.id);',
    "%}",
    "",
    "### Next request",
    "GET https://example.invalid/items/1",
  ].join("\n");
  const validWorkspace = new WorkspaceIndex();
  const validIndex = validWorkspace.update("file:///C:/work/valid-comments.http", validText);
  const validDiagnostics = await diagnostics(validWorkspace, validIndex, knownMetadata, knownVariables);
  assert(!validDiagnostics.some((item) => item.code === "invalid-json"));

  const invalidText = [
    "POST https://example.invalid/inline-comment",
    "Content-Type: application/json",
    "",
    "{",
    '  "value": 1 // inline comments are sent by httpYac',
    "}",
    "### Trailing comma",
    "POST https://example.invalid/trailing-comma",
    "Content-Type: application/json",
    "",
    "{",
    '  "value": 1,',
    "}",
  ].join("\n");
  const invalidWorkspace = new WorkspaceIndex();
  const invalidIndex = invalidWorkspace.update("file:///C:/work/invalid-jsonc.http", invalidText);
  const invalidDiagnostics = await diagnostics(invalidWorkspace, invalidIndex, knownMetadata, knownVariables);
  assert.equal(invalidDiagnostics.filter((item) => item.code === "invalid-json").length, 2);
});

test("reports request names duplicated across the import graph", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "httpyac-lsp-duplicates-"));
  const importedPath = path.join(directory, "imported.http");
  const mainPath = path.join(directory, "main.http");
  await fs.writeFile(
    importedPath,
    "# @name sharedName\nGET https://example.invalid/imported\n",
    "utf8",
  );
  const mainText = [
    "# @import ./imported.http",
    "# @name sharedName",
    "GET https://example.invalid/main",
  ].join("\n");
  const workspace = new WorkspaceIndex();
  const index = workspace.update(pathToFileURL(mainPath).toString(), mainText);
  const catalog = loadSafeHttpYacCatalog();
  const found = await diagnostics(
    workspace,
    index,
    new Set(catalog.metadata.map((item) => item.name.toLowerCase())),
    catalogVariableNames(catalog),
  );
  assert(found.some((item) => item.code === "duplicate-name"));
});

test("renames spaced request names and their normalized response variables safely", async () => {
  const text = [
    "// @name=My Login",
    "GET https://example.invalid/login",
    "###",
    "// @ref=My Login",
    "GET https://example.invalid/me",
    "Authorization: Bearer {{MyLogin.token}}",
    "# @ref {{dynamicRequest}}",
    "GET https://example.invalid/dynamic",
  ].join("\n");
  const workspace = new WorkspaceIndex();
  const index = workspace.update("file:///C:/work/names.http", text);
  const catalog = loadSafeHttpYacCatalog();
  const found = await diagnostics(
    workspace,
    index,
    new Set(catalog.metadata.map((item) => item.name.toLowerCase())),
    catalogVariableNames(catalog),
  );
  assert(!found.some((item) => item.code === "invalid-name"));
  assert(!found.some((item) =>
    item.code === "undefined-ref" && item.message.includes("dynamicRequest")
  ));
  assert.equal(references(workspace, index, { line: 0, character: 12 }).length, 3);
  const edit = rename(workspace, index, { line: 0, character: 12 }, "New Session");
  const changes = edit?.changes?.[index.uri] ?? [];
  assert.equal(changes.filter((item) => item.newText === "New Session").length, 2);
  assert.equal(changes.filter((item) => item.newText === "NewSession").length, 1);
  assert(changes.some((item) =>
    item.newText === "NewSession" &&
    item.range.end.character - item.range.start.character === "MyLogin".length
  ));
});

test("filters method, metadata, variable, header, and path completions by context", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "httpyac-lsp-completion-"));
  await fs.writeFile(path.join(directory, "shared.http"), "GET https://example.invalid\n", "utf8");
  const workspace = new WorkspaceIndex();
  const catalog = loadSafeHttpYacCatalog();

  const methodIndex = workspace.update(
    pathToFileURL(path.join(directory, "method.http")).toString(),
    "",
  );
  const methods = await completions(workspace, methodIndex, { line: 0, character: 0 }, catalog);
  assert(methods.some((item) => item.label === "GET"));

  const metadataIndex = workspace.update(
    pathToFileURL(path.join(directory, "metadata.http")).toString(),
    "# @",
  );
  const metadata = await completions(workspace, metadataIndex, { line: 0, character: 3 }, catalog);
  assert(metadata.some((item) => item.label === "@name"));

  const variableText = "baseUrl = https://example.invalid\nGET {{";
  const variableIndex = workspace.update(
    pathToFileURL(path.join(directory, "variable.http")).toString(),
    variableText,
  );
  const variables = await completions(workspace, variableIndex, { line: 1, character: 6 }, catalog);
  assert(variables.some((item) => item.label === "baseUrl"));

  const headerText = "GET https://example.invalid\nAuth";
  const headerIndex = workspace.update(
    pathToFileURL(path.join(directory, "header.http")).toString(),
    headerText,
  );
  const headers = await completions(workspace, headerIndex, { line: 1, character: 4 }, catalog);
  assert(headers.some((item) => item.label === "Authorization"));
  assert(!headers.some((item) => item.label === "GET"), "method completion must be filtered in headers");

  const pathText = "# @import ./sh";
  const pathIndex = workspace.update(
    pathToFileURL(path.join(directory, "path.http")).toString(),
    pathText,
  );
  const paths = await completions(workspace, pathIndex, { line: 0, character: pathText.length }, catalog);
  assert(paths.some((item) => item.label === "shared.http"));
});

test("offers all roadmap quick fixes and preserves injected content while formatting", async () => {
  const text = [
    "baseUrl = https://example.invalid   ",
    "# @name login",
    "GET {{baseUrl}}/login",
    "###",
    "# @ref logn",
    "POST {{baseUrll}}/profile   ",
    "",
    '{ "body": true }   ',
  ].join("\n");
  const workspace = new WorkspaceIndex();
  const index = workspace.update("file:///C:/work/actions.http", text);
  const catalog = loadSafeHttpYacCatalog();
  const found = await diagnostics(
    workspace,
    index,
    new Set(catalog.metadata.map((item) => item.name.toLowerCase())),
    catalogVariableNames(catalog),
  );
  const actions = codeActions(
    index,
    found,
    { start: { line: 5, character: 0 }, end: { line: 7, character: 18 } },
  );
  assert(actions.some((item) => item.title === "改为 @ref login"));
  assert(actions.some((item) => item.title === "改为变量 baseUrl"));
  assert(actions.some((item) => item.title === "为请求创建 @name"));
  assert(actions.some((item) => item.title === "添加 JSON Content-Type/Accept headers"));

  const edits = formattingEdits(index);
  assert(edits.some((item) => item.range.start.line === 0), "outer trailing whitespace should be removed");
  assert(!edits.some((item) => item.range.start.line === 7), "body bytes must remain untouched");
});
