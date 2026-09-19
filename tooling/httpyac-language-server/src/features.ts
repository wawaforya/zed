import { promises as fs } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  CodeAction,
  CodeActionKind,
  CompletionItem,
  CompletionItemKind,
  Diagnostic,
  DiagnosticSeverity,
  DocumentLink,
  Hover,
  Location,
  MarkupKind,
  Position,
  Range,
  SymbolInformation,
  SymbolKind,
  TextEdit,
  WorkspaceEdit,
} from "vscode-languageserver";
import { HTTP_METHODS, responseVariableName, wordAt } from "./indexer";
import type { DocumentIndex } from "./model";
import type { HttpYacStaticCatalog } from "./security";
import { resolvePath, WorkspaceIndex } from "./workspace";

const KNOWN_BUILTINS = new Set([
  "$shared", "$processEnv", "$dotenv", "$random", "$timestamp", "$isoTimestamp",
  "request", "response", "client", "exports", "module",
]);
const knownHeaders = [
  "Accept", "Accept-Encoding", "Authorization", "Cache-Control", "Content-Type",
  "Cookie", "If-Match", "Origin", "Referer", "User-Agent", "X-API-Key",
  "X-Auth-Token", "X-Request-ID",
];

function linePrefix(index: DocumentIndex, position: Position): string {
  return (index.lines[position.line] ?? "").slice(0, position.character);
}

function dedupe(items: CompletionItem[]): CompletionItem[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    const key = String(item.label).toLowerCase();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function staticItems(
  entries: Array<{ name: string; description?: string; text?: string }>,
  kind: CompletionItemKind,
): CompletionItem[] {
  return entries.map((entry) => ({
    label: entry.name,
    kind,
    detail: entry.description,
    documentation: entry.description,
    insertText: entry.text ?? entry.name,
  }));
}

export async function completions(
  workspace: WorkspaceIndex,
  index: DocumentIndex,
  position: Position,
  catalog: HttpYacStaticCatalog,
): Promise<CompletionItem[]> {
  const prefix = linePrefix(index, position);
  const reachable = await workspace.reachable(index);
  const requestAtLine = index.requests.find((request) =>
    request.startLine < position.line && request.endLine >= position.line
  );

  if (/^\s*#\s*@(?:forceRef|ref)\s+\S*$/i.test(prefix)) {
    return dedupe(reachable.flatMap((document) => document.requests)
      .filter((request) => request.explicitName)
      .map((request) => ({
        label: request.name,
        kind: CompletionItemKind.Reference,
        detail: `${request.method ?? "REQUEST"} ${request.url ?? ""}`.trim(),
        documentation: request.description,
      })));
  }

  if (/\{\{\s*[\w.$-]*$/.test(prefix)) {
    const localItems: CompletionItem[] = [
      ...reachable.flatMap((document) => document.variables).map((variable) => ({
        label: variable.name,
        kind: CompletionItemKind.Variable,
        detail: "httpYac file variable",
      })),
      ...reachable.flatMap((document) => document.requests)
        .filter((request) => request.explicitName && request.responseVariableName)
        .map((request) => ({
          label: request.responseVariableName ?? request.name,
          kind: CompletionItemKind.Variable,
          detail: "Named response",
        })),
    ];
    return dedupe([...localItems, ...staticItems(catalog.variables, CompletionItemKind.Variable)]);
  }

  if (/^\s*#\s*@[\w-]*$/.test(prefix)) {
    return dedupe(catalog.metadata.map((entry) => ({
      label: `@${entry.name}`,
      kind: CompletionItemKind.Property,
      detail: entry.description,
      documentation: entry.description,
      insertText: `@${entry.name}${entry.completions?.length ? " " : ""}`,
    })));
  }

  if (/^\s*(?:#\s*@(?:import|proto)\s+|proto\s*<\s*|[<>]\s+)\S*$/i.test(prefix)) {
    return await pathCompletions(index, prefix);
  }

  if (!requestAtLine && /^\s*[A-Za-z-]*$/.test(prefix)) {
    return [
      ...HTTP_METHODS.map((method) => ({
        label: method,
        kind: CompletionItemKind.Keyword,
        detail: "HTTP/httpYac request method",
        insertText: `${method} `,
      })),
      ...staticItems(catalog.emptyLine, CompletionItemKind.Snippet),
    ];
  }

  if (requestAtLine && (/^\s*[\w-]*$/.test(prefix) || /^\s*[\w-]+\s*:\s*.*$/.test(prefix))) {
    return dedupe([
      ...knownHeaders.map((header) => ({
        label: header,
        kind: CompletionItemKind.Field,
        insertText: `${header}: `,
      })),
      ...staticItems(catalog.headers, CompletionItemKind.Field),
    ]);
  }
  return [];
}

async function pathCompletions(index: DocumentIndex, prefix: string): Promise<CompletionItem[]> {
  const uriPath = resolvePath(index.uri, ".");
  if (!uriPath) return [];
  const typed = prefix.match(/(?:\s|<)([^\s]*)$/)?.[1] ?? "";
  const directoryPart = typed.replace(/[\\/][^\\/]*$/, "") || ".";
  const base = path.resolve(uriPath, directoryPart);
  try {
    const entries = await fs.readdir(base, { withFileTypes: true });
    return entries.map((entry) => ({
      label: entry.name + (entry.isDirectory() ? path.sep : ""),
      kind: entry.isDirectory() ? CompletionItemKind.Folder : CompletionItemKind.File,
    }));
  } catch {
    return [];
  }
}

export async function diagnostics(
  workspace: WorkspaceIndex,
  index: DocumentIndex,
  knownMetadata: Set<string>,
  knownVariables: Set<string> = new Set(),
): Promise<Diagnostic[]> {
  const result: Diagnostic[] = [];
  const reachable = await workspace.reachable(index);
  const allNamedRequests = reachable
    .flatMap((document) => document.requests)
    .filter((request) => request.explicitName);
  for (const request of index.requests.filter((item) => item.explicitName)) {
    if (!isValidRequestName(request.name)) {
      result.push(error(request.range, `非法的 request name: ${request.name}`, "invalid-name"));
    }
    if (allNamedRequests.filter((item) => item.name === request.name).length > 1) {
      result.push(error(
        request.range,
        `当前文件或导入图中存在重复的 request name: ${request.name}`,
        "duplicate-name",
      ));
    }
  }

  for (const reference of index.requestReferences) {
    if (reference.name.includes("{{")) continue;
    if ((await workspace.requestDefinitions(index, reference.name)).length === 0) {
      result.push(error(reference.range, `未定义的 request reference: ${reference.name}`, "undefined-ref"));
    }
  }

  for (const reference of index.variableReferences) {
    const rootName = reference.rootName;
    if (KNOWN_BUILTINS.has(rootName) || knownVariables.has(reference.name) || knownVariables.has(rootName)) {
      continue;
    }
    if ((await workspace.variableDefinitions(index, reference.name)).length === 0) {
      result.push(warning(
        reference.rootRange,
        `未定义的变量: ${reference.rootName}`,
        "undefined-variable",
      ));
    }
  }

  for (const request of index.requests) {
    if (request.method && !HTTP_METHODS.includes(request.method.toUpperCase() as typeof HTTP_METHODS[number])) {
      result.push(error(request.methodRange ?? request.range, `不支持的方法: ${request.method}`, "invalid-method"));
    }
    if (request.url && !isValidUrl(request.url)) {
      result.push(warning(request.urlRange ?? request.range, `URL 看起来不完整: ${request.url}`, "invalid-url"));
    }
  }

  for (const item of index.metadata) {
    if (!knownMetadata.has(item.name.toLowerCase())) {
      result.push(warning(item.range, `未知的 httpYac metadata: @${item.name}`, "unknown-metadata"));
    }
    if (["name", "ref", "forceref", "import"].includes(item.name.toLowerCase()) && !item.value) {
      result.push(error(item.range, `@${item.name} 需要参数`, "missing-metadata-value"));
    }
    const parameterError = validateMetadataParameter(item.name, item.value);
    if (parameterError && item.valueRange) {
      result.push(error(item.valueRange, parameterError, "invalid-metadata-value"));
    }
  }

  for (const reference of index.paths) {
    const fileName = resolvePath(index.uri, reference.path);
    if (!fileName) continue;
    try {
      await fs.access(fileName);
    } catch {
      result.push(error(reference.range, `文件不存在: ${reference.path}`, "missing-file"));
    }
  }

  result.push(...await cycleDiagnostics(workspace, index));
  result.push(...await referenceCycleDiagnostics(workspace, index));
  for (const body of index.jsonBodies) {
    if (body.text.includes("{{")) continue;
    try {
      JSON.parse(body.text);
    } catch (cause) {
      result.push(error(body.range, `JSON body 无效: ${(cause as Error).message}`, "invalid-json"));
    }
  }
  return result;
}

function validateMetadataParameter(name: string, value: string | undefined): string | undefined {
  if (!value) return undefined;
  switch (name.toLowerCase()) {
    case "name":
      return isValidRequestName(value)
        ? undefined
        : "@name 参数包含非法字符";
    case "ref":
    case "forceref":
      return isValidRequestName(value) || value.includes("{{")
        ? undefined
        : `@${name} 参数包含非法字符`;
    case "timeout":
      return /^\s*\d+(?:\.\d+)?(?:\s*ms)?\s*$/i.test(value) || value.includes("{{")
        ? undefined
        : "@timeout 参数必须以毫秒数开头";
    case "loop":
      return /^\s*(?:for\s+(?:\d+|.+\s+of\s+.+)|while\s+.+)\s*$/i.test(value)
        ? undefined
        : "@loop 参数应为 `for N`、`for value of iterable` 或 `while expression`";
    case "ratelimit":
      return /^\s*(?:(?:slot\s*:?\s*\S+)\s*)?(?:(?:minIdleTime\s*:?\s*\d+)\s*)?(?:max\s*:?\s*\d+)(?:\s+expire\s*:?\s*\d+)?\s*$/i.test(value)
        ? undefined
        : "@ratelimit 参数格式无效";
    default:
      return undefined;
  }
}

function isValidRequestName(name: string): boolean {
  return /^[^\s#][^#\r\n]*$/.test(name.trim());
}

async function cycleDiagnostics(workspace: WorkspaceIndex, start: DocumentIndex): Promise<Diagnostic[]> {
  const stack = new Set<string>();
  const visited = new Set<string>();
  const result: Diagnostic[] = [];
  const visit = async (index: DocumentIndex): Promise<void> => {
    if (stack.has(index.uri) || visited.has(index.uri)) return;
    stack.add(index.uri);
    for (const reference of index.paths.filter((item) => item.kind === "import")) {
      const fileName = resolvePath(index.uri, reference.path);
      if (!fileName) continue;
      const uri = pathToFileURL(fileName).toString();
      if (stack.has(uri)) {
        result.push(error(reference.range, `检测到 import 循环: ${reference.path}`, "import-cycle"));
        continue;
      }
      const imported = await workspace.loadFile(fileName);
      if (imported) await visit(imported);
    }
    stack.delete(index.uri);
    visited.add(index.uri);
  };
  await visit(start);
  return result;
}

async function referenceCycleDiagnostics(
  workspace: WorkspaceIndex,
  start: DocumentIndex,
): Promise<Diagnostic[]> {
  const documents = await workspace.reachable(start);
  const requests = documents.flatMap((document) => document.requests);
  const byName = new Map(
    requests
      .filter((request) => request.explicitName)
      .map((request) => [request.name, request]),
  );
  const result: Diagnostic[] = [];
  const visited = new Set<string>();
  const stack = new Set<string>();
  const visit = (name: string): void => {
    if (stack.has(name) || visited.has(name)) return;
    const request = byName.get(name);
    if (!request) return;
    stack.add(name);
    for (const reference of request.references) {
      if (stack.has(reference.name)) {
        result.push(error(
          reference.range,
          `检测到 request reference 循环: ${reference.name}`,
          "reference-cycle",
        ));
      } else {
        visit(reference.name);
      }
    }
    stack.delete(name);
    visited.add(name);
  };
  for (const name of byName.keys()) visit(name);
  return result;
}

function isValidUrl(value: string): boolean {
  if (value.includes("{{")) return true;
  if (/^(?:https?|wss?|grpc|mqtts?|amqp):\/\/\S+$/i.test(value)) return true;
  return value.startsWith("/") || value.startsWith(":");
}

function error(range: Range, message: string, code: string): Diagnostic {
  return { range, message, code, source: "httpyac", severity: DiagnosticSeverity.Error };
}

function warning(range: Range, message: string, code: string): Diagnostic {
  return { range, message, code, source: "httpyac", severity: DiagnosticSeverity.Warning };
}

export async function definition(
  workspace: WorkspaceIndex,
  index: DocumentIndex,
  position: Position,
): Promise<Location[]> {
  const pathReference = workspace.findPath(index, position.line, position.character);
  if (pathReference) {
    const fileName = resolvePath(index.uri, pathReference.path);
    return fileName ? [Location.create(pathToFileURL(fileName).toString(), Range.create(0, 0, 0, 0))] : [];
  }
  const occurrence = wordAt(index, position);
  if (!occurrence) return [];
  if (index.requestReferences.includes(occurrence)) {
    return workspace.requestDefinitions(index, occurrence.name);
  }
  const variableReference = index.variableReferences.find((item) => item === occurrence);
  const responseRequestName = variableReference
    ? workspace.requestNameForResponseVariable(variableReference.rootName)
    : undefined;
  if (responseRequestName) {
    return workspace.requestDefinitions(index, responseRequestName);
  }
  return workspace.variableDefinitions(index, occurrence.name);
}

export function references(
  workspace: WorkspaceIndex,
  index: DocumentIndex,
  position: Position,
): Location[] {
  const occurrence = wordAt(index, position);
  if (!occurrence) return [];
  const variableReference = index.variableReferences.find((item) => item === occurrence);
  const requestName = index.requests.includes(occurrence as never) ||
    index.requestReferences.includes(occurrence)
    ? occurrence.name
    : variableReference
      ? workspace.requestNameForResponseVariable(variableReference.rootName)
      : undefined;
  const responseVariable = requestName
    ? workspace.responseVariableForRequest(requestName)
    : undefined;
  const matches = requestName
    ? [
        ...workspace.requestOccurrences(requestName),
        ...(responseVariable
          ? workspace.responseVariableOccurrences(responseVariable)
          : []),
      ]
    : workspace.variableOccurrences(
        variableReference?.rootName ?? occurrence.name,
      );
  return matches.map((item) => Location.create(item.uri, item.occurrence.range));
}

export function rename(
  workspace: WorkspaceIndex,
  index: DocumentIndex,
  position: Position,
  newName: string,
): WorkspaceEdit | null {
  const occurrence = wordAt(index, position);
  if (!occurrence) return null;
  const variableReference = index.variableReferences.find((item) => item === occurrence);
  const requestName = index.requests.includes(occurrence as never) ||
    index.requestReferences.includes(occurrence)
    ? occurrence.name
    : variableReference
      ? workspace.requestNameForResponseVariable(variableReference.rootName)
      : undefined;
  if (requestName && !isValidRequestName(newName)) return null;
  if (!requestName && !/^[A-Za-z_][\w.-]*$/.test(newName)) return null;
  const changes: Record<string, TextEdit[]> = {};
  if (requestName) {
    for (const item of workspace.requestOccurrences(requestName)) {
      (changes[item.uri] ??= []).push(TextEdit.replace(item.occurrence.range, newName));
    }
    const responseVariable = workspace.responseVariableForRequest(requestName);
    if (responseVariable) {
      const newResponseVariable = responseVariableName(newName);
      for (const item of workspace.responseVariableOccurrences(responseVariable)) {
        (changes[item.uri] ??= []).push(
          TextEdit.replace(item.occurrence.range, newResponseVariable),
        );
      }
    }
  } else {
    const variableName = variableReference?.rootName ?? occurrence.name;
    for (const item of workspace.variableOccurrences(variableName)) {
      (changes[item.uri] ??= []).push(TextEdit.replace(item.occurrence.range, newName));
    }
  }
  return { changes };
}

export function documentLinks(index: DocumentIndex): DocumentLink[] {
  return index.paths.flatMap((reference) => {
    const fileName = resolvePath(index.uri, reference.path);
    return fileName
      ? [{ range: reference.range, target: pathToFileURL(fileName).toString(), tooltip: `${reference.kind} file` }]
      : [];
  });
}

export function formattingEdits(index: DocumentIndex): TextEdit[] {
  const edits: TextEdit[] = [];
  for (let line = 0; line < index.lines.length; line++) {
    if (index.protectedLines.has(line)) continue;
    const source = index.lines[line];
    const trimmed = source.trimEnd();
    if (trimmed.length !== source.length) {
      edits.push(TextEdit.del({
        start: { line, character: trimmed.length },
        end: { line, character: source.length },
      }));
    }
  }
  return edits;
}

export function workspaceSymbols(workspace: WorkspaceIndex, query: string): SymbolInformation[] {
  const lowerQuery = query.toLowerCase();
  return workspace.all().flatMap((index) =>
    index.requests
      .filter((request) =>
        request.explicitName && request.name.toLowerCase().includes(lowerQuery)
      )
      .map((request) => SymbolInformation.create(
        request.name,
        SymbolKind.Method,
        request.range,
        index.uri,
        `${request.method ?? ""} ${request.url ?? ""}`.trim(),
      ))
  );
}

export function hover(index: DocumentIndex, position: Position, catalog: HttpYacStaticCatalog): Hover | null {
  const metadata = index.metadata.find((item) =>
    item.range.start.line === position.line &&
    item.range.start.character <= position.character &&
    item.range.end.character >= position.character
  );
  if (metadata) {
    const known = catalog.metadata.find((item) => item.name.toLowerCase() === metadata.name.toLowerCase());
    return known
      ? { contents: { kind: MarkupKind.Markdown, value: `**@${known.name}**\n\n${known.description ?? "httpYac metadata"}` } }
      : null;
  }
  const request = index.requests.find((item) =>
    item.startLine === position.line &&
    item.methodRange &&
    item.methodRange.start.character <= position.character &&
    item.methodRange.end.character >= position.character
  );
  if (request) {
    return {
      contents: {
        kind: MarkupKind.Markdown,
        value: `**${request.method}** request\n\n${request.description ?? request.url ?? "httpYac request"}`,
      },
    };
  }
  const variable = index.variableReferences.find(({ range }) =>
    range.start.line === position.line &&
    range.start.character <= position.character &&
    range.end.character >= position.character
  );
  if (variable) {
    const known = catalog.variables.find((item) =>
      normalizeCatalogVariable(item.name) === variable.name ||
      normalizeCatalogVariable(item.name) === variable.rootName
    );
    const request = index.requests.find((item) =>
      item.responseVariableName === variable.rootName
    );
    const definition = index.variables.find((item) =>
      variable.name === item.name || variable.name.startsWith(`${item.name}.`)
    );
    const description = known?.description ??
      (request
        ? `Named response from ${request.method ?? "request"} ${request.url ?? ""}`.trim()
        : definition
          ? "httpYac file variable"
          : undefined);
    return description
      ? {
          contents: {
            kind: MarkupKind.Markdown,
            value: `**${variable.name}**\n\n${description}`,
          },
        }
      : null;
  }
  const line = index.lines[position.line] ?? "";
  const header = line.match(/^\s*([\w-]+)\s*:/)?.[1];
  if (header) {
    const known = catalog.headers.find((item) =>
      item.name.toLowerCase() === header.toLowerCase()
    );
    return {
      contents: {
        kind: MarkupKind.Markdown,
        value: `**${header}** HTTP header\n\n${known?.description ?? "Header names are case-insensitive."}`,
      },
    };
  }
  return null;
}

export function catalogVariableNames(catalog: HttpYacStaticCatalog): Set<string> {
  return new Set(catalog.variables.map((item) => normalizeCatalogVariable(item.name)));
}

function normalizeCatalogVariable(name: string): string {
  return name.trim().split(/\s/)[0].replace(/\(\)$/, "");
}

export function codeActions(
  index: DocumentIndex,
  diagnostics: Diagnostic[],
  requestedRange?: Range,
): CodeAction[] {
  const actions: CodeAction[] = [];
  for (const diagnostic of diagnostics) {
    if (diagnostic.code === "undefined-ref") {
      const name = index.lines[diagnostic.range.start.line]?.slice(
        diagnostic.range.start.character,
        diagnostic.range.end.character,
      );
      const similar = nearest(
        name,
        index.requests.filter((request) => request.explicitName).map((request) => request.name),
      );
      if (similar) {
        actions.push({
          title: `改为 @ref ${similar}`,
          kind: CodeActionKind.QuickFix,
          diagnostics: [diagnostic],
          edit: { changes: { [index.uri]: [TextEdit.replace(diagnostic.range, similar)] } },
        });
      }
    }
    if (diagnostic.code === "undefined-variable") {
      const name = index.lines[diagnostic.range.start.line]?.slice(
        diagnostic.range.start.character,
        diagnostic.range.end.character,
      );
      const similar = nearest(name, index.variables.map((item) => item.name));
      if (similar) {
        actions.push({
          title: `改为变量 ${similar}`,
          kind: CodeActionKind.QuickFix,
          diagnostics: [diagnostic],
          edit: { changes: { [index.uri]: [TextEdit.replace(diagnostic.range, similar)] } },
        });
      }
    }
  }

  const request = index.requests.find((item) =>
    !item.explicitName &&
    requestedRange &&
    item.startLine <= requestedRange.start.line &&
    item.endLine >= requestedRange.end.line
  ) ?? index.requests.find((item) => !item.explicitName);
  if (request && !request.explicitName) {
    actions.push({
      title: "为请求创建 @name",
      kind: CodeActionKind.QuickFix,
      edit: {
        changes: {
          [index.uri]: [TextEdit.insert(Position.create(request.startLine, 0), "# @name requestName\n")],
        },
      },
    });
  }

  const position = Position.create(0, 0);
  const headerRequest = index.requests.find((item) =>
    requestedRange &&
    item.startLine <= requestedRange.start.line &&
    item.endLine >= requestedRange.end.line
  ) ?? index.requests[0];
  const requestLines = headerRequest
    ? index.lines.slice(headerRequest.startLine + 1, headerRequest.endLine + 1)
    : index.lines;
  if (!requestLines.some((line) => /^\s*Content-Type\s*:/i.test(line))) {
    actions.push({
      title: "添加 JSON Content-Type/Accept headers",
      kind: CodeActionKind.QuickFix,
      edit: {
        changes: {
          [index.uri]: [TextEdit.insert(
            headerRequest ? Position.create(headerRequest.startLine + 1, 0) : position,
            "Content-Type: application/json\nAccept: application/json\n",
          )],
        },
      },
    });
  }
  return actions;
}

function nearest(source: string | undefined, candidates: string[]): string | undefined {
  if (!source) return undefined;
  const scored = candidates
    .filter((candidate) => candidate !== source)
    .map((candidate) => ({ candidate, score: distance(source, candidate) }))
    .sort((left, right) => left.score - right.score);
  return scored[0]?.score <= Math.max(2, Math.floor(source.length / 3)) ? scored[0].candidate : undefined;
}

function distance(left: string, right: string): number {
  const rows = Array.from({ length: left.length + 1 }, (_, index) => index);
  for (let column = 1; column <= right.length; column++) {
    let previous = rows[0];
    rows[0] = column;
    for (let row = 1; row <= left.length; row++) {
      const old = rows[row];
      rows[row] = Math.min(
        rows[row] + 1,
        rows[row - 1] + 1,
        previous + (left[row - 1] === right[column - 1] ? 0 : 1),
      );
      previous = old;
    }
  }
  return rows[left.length];
}
