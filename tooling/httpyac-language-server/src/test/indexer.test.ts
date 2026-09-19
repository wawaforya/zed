import assert from "node:assert/strict";
import test from "node:test";
import { indexDocument } from "../indexer";

test("indexes requests, references, variables, paths, and JSON without executing scripts", () => {
  const index = indexDocument("file:///C:/work/main.http", [
    "baseUrl = https://example.invalid",
    "# @import ./shared.http",
    "# @name login",
    "POST {{baseUrl}}/login",
    "Content-Type: application/json",
    "",
    '{"name":"demo"}',
    "",
    "> {% client.global.set('token', response.body.token); %}",
    "###",
    "# @ref login",
    "GET {{baseUrl}}/me",
    "Authorization: Bearer {{login.token}}",
  ].join("\n"));

  assert.equal(index.requests.length, 2);
  assert.equal(index.requests[0].name, "login");
  assert.deepEqual(index.requestReferences.map((item) => item.name), ["login"]);
  assert.deepEqual(index.variables.map((item) => item.name), ["baseUrl"]);
  assert.deepEqual(index.paths.map((item) => [item.kind, item.path]), [["import", "./shared.http"]]);
  assert.equal(index.jsonBodies.length, 1);
  assert(index.protectedLines.has(6), "injected body must be protected from formatting");
  assert(index.protectedLines.has(8), "script line must be protected from formatting");
});

test("delimits JSON before handlers and normalizes only httpYac body comments", () => {
  const index = indexDocument("file:///C:/work/comments.http", [
    "# @name createItem",
    "POST https://example.invalid/items",
    "Content-Type: application/json",
    "",
    "{",
    "  // visible note",
    '  "first": 1,',
    "  /*",
    "   * another note",
    "   */",
    '  "second": 2',
    "}",
    "",
    "> {%",
    'client.global.set("created", response.body.id);',
    "%}",
    "",
    "### Read the created item",
    "# @ref createItem",
    "GET https://example.invalid/items/1",
  ].join("\n"));

  assert.equal(index.requests[0].endLine, 16);
  assert.equal(index.jsonBodies.length, 1);
  assert.equal(index.jsonBodies[0].range.start.line, 4);
  assert.equal(index.jsonBodies[0].range.end.line, 11);
  assert.deepEqual(JSON.parse(index.jsonBodies[0].text), { first: 1, second: 2 });
  assert(!index.jsonBodies[0].text.includes("client.global"));
  assert(!index.jsonBodies[0].text.includes("###"));

  const inlineComment = indexDocument("file:///C:/work/inline-comment.http", [
    "POST https://example.invalid/items",
    "Content-Type: application/json",
    "",
    "{",
    '  "value": 1 // not removed by httpYac',
    "}",
  ].join("\n"));
  assert.match(inlineComment.jsonBodies[0].text, /\/\/ not removed/);
  assert.throws(() => JSON.parse(inlineComment.jsonBodies[0].text));
});

test("does not interpret request-like lines inside native scripts", () => {
  const index = indexDocument("file:///C:/work/script.http", [
    "{{",
    "  GET https://must-not-be-a-request.invalid",
    "}}",
    "GET https://example.invalid",
  ].join("\n"));
  assert.equal(index.requests.length, 1);
  assert.equal(index.requests[0].url, "https://example.invalid");
});

test("supports slash metadata, equals syntax, and httpYac response-name normalization", () => {
  const index = indexDocument("file:///C:/work/names.http", [
    "// @name=My Login",
    "GET https://example.invalid/login",
    "###",
    "// @ref=My Login",
    "GET https://example.invalid/me",
    "Authorization: Bearer {{MyLogin.token}}",
  ].join("\n"));
  assert.equal(index.requests[0].explicitName, true);
  assert.equal(index.requests[0].name, "My Login");
  assert.equal(index.requests[0].responseVariableName, "MyLogin");
  assert.deepEqual(index.requestReferences.map((item) => item.name), ["My Login"]);
  assert.equal(index.variableReferences[0].rootName, "MyLogin");
});
