import {
  Position,
  Range,
} from "vscode-languageserver";
import type {
  DocumentIndex,
  MetadataEntry,
  Occurrence,
  PathKind,
  PathReference,
  RequestDefinition,
  VariableReference,
} from "./model";

export const HTTP_METHODS = [
  "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "CONNECT",
  "TRACE", "PROPFIND", "PROPPATCH", "MKCOL", "COPY", "MOVE", "LOCK",
  "UNLOCK", "CHECKOUT", "CHECKIN", "REPORT", "MERGE", "MKACTIVITY",
  "MKWORKSPACE", "VERSION-CONTROL", "BASELINE-CONTROL", "MKCALENDAR",
  "ACL", "SEARCH", "GRAPHQL", "GRPC", "WS", "WSS", "WEBSOCKET", "SSE",
  "EVENTSOURCE", "MQTT", "MQTTS", "AMQP",
] as const;

const methodSet = new Set<string>(HTTP_METHODS);
const metadataPattern = /^\s*(?:#|\/\/)\s*@([A-Za-z][\w-]*)(?:(?:\s+|=\s*)(.+?))?\s*$/;
const requestPattern = /^\s*([A-Za-z][A-Za-z-]*)\s+(\S+)(?:\s+HTTP\/\d(?:\.\d)?)?\s*$/;
const variablePattern = /^\s*@?([A-Za-z_][\w.-]*)\s*(=|:=)\s*(.*?)\s*$/;
const regionSeparatorPattern = /^\s*#{3,}(?:\s+.*)?$/;
const pathMetadata = new Map<string, PathKind>([
  ["import", "import"],
  ["proto", "proto"],
]);

function range(line: number, start: number, end: number): Range {
  return Range.create(Position.create(line, start), Position.create(line, end));
}

function valueRange(line: number, source: string, value: string): Range {
  const start = source.indexOf(value);
  return range(line, Math.max(0, start), Math.max(0, start) + value.length);
}

function unquote(value: string): string {
  const trimmed = value.trim();
  if (
    (trimmed.startsWith('"') && trimmed.endsWith('"')) ||
    (trimmed.startsWith("'") && trimmed.endsWith("'"))
  ) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function isPostBodySyntax(source: string): boolean {
  return regionSeparatorPattern.test(source) ||
    metadataPattern.test(source) ||
    /^\s*\?\?/.test(source) ||
    /^\s*>>!?/.test(source) ||
    /^\s*>\s*(?:\{%|.+\.(?:js|mjs|cjs|ts)\s*$)/i.test(source) ||
    /^\s*HTTP\/\d(?:\.\d)?\s+\d{3}\b/i.test(source) ||
    /^\s*\{\{\s*[@+](?:request|response|streaming)\b/i.test(source);
}

function isHttpYacLineComment(source: string): boolean {
  return Boolean(/^\s*\/\/\s*(.*)\s*$/.exec(source)?.[1]);
}

function findHttpYacBlockCommentEnd(
  lines: string[],
  startLine: number,
  endLine = lines.length - 1,
): number | undefined {
  if (!/^\s*\/\*$/.test(lines[startLine])) return undefined;
  for (let line = startLine + 1; line <= endLine; line++) {
    if (/^\s*\*\/\s*$/.test(lines[line])) return line;
  }
  return undefined;
}

function findJsonContentStartLine(
  lines: string[],
  bodyStart: number,
  requestEnd: number,
): number | undefined {
  for (let line = bodyStart; line <= requestEnd; line++) {
    if (lines[line].trim() === "" || isHttpYacLineComment(lines[line])) continue;
    const blockCommentEnd = findHttpYacBlockCommentEnd(lines, line, requestEnd);
    if (blockCommentEnd !== undefined) {
      line = blockCommentEnd;
      continue;
    }
    return line;
  }
  return undefined;
}

function findJsonBodyEndLine(lines: string[], bodyStart: number, requestEnd: number): number {
  const stack: string[] = [];
  let started = false;
  let inString = false;
  let escaped = false;
  let inBlockComment = false;
  let inHandlebars = false;

  for (let lineNumber = bodyStart; lineNumber <= requestEnd; lineNumber++) {
    const source = lines[lineNumber];
    if (lineNumber > bodyStart && isPostBodySyntax(source)) {
      return Math.max(bodyStart, lineNumber - 1);
    }

    for (let offset = 0; offset < source.length; offset++) {
      const char = source[offset];
      const next = source[offset + 1];

      if (inHandlebars) {
        if (char === "}" && next === "}") {
          inHandlebars = false;
          offset++;
        }
        continue;
      }
      if (inBlockComment) {
        if (char === "*" && next === "/") {
          inBlockComment = false;
          offset++;
        }
        continue;
      }
      if (inString) {
        if (escaped) {
          escaped = false;
        } else if (char === "\\") {
          escaped = true;
        } else if (char === '"') {
          inString = false;
        }
        continue;
      }
      if (char === '"') {
        inString = true;
        continue;
      }
      if (char === "/" && next === "/") break;
      if (char === "/" && next === "*") {
        inBlockComment = true;
        offset++;
        continue;
      }
      if (char === "{" && next === "{") {
        inHandlebars = true;
        offset++;
        continue;
      }
      if (char === "{" || char === "[") {
        stack.push(char);
        started = true;
        continue;
      }
      if (char === "}" || char === "]") {
        const expected = char === "}" ? "{" : "[";
        if (stack[stack.length - 1] === expected) stack.pop();
        if (started && stack.length === 0) return lineNumber;
      }
    }
  }
  return requestEnd;
}

function normalizeJsonBodyComments(text: string): string {
  const lines = text.split("\n");
  for (let line = 0; line < lines.length; line++) {
    if (isHttpYacLineComment(lines[line])) {
      lines[line] = "";
      continue;
    }
    const closingLine = findHttpYacBlockCommentEnd(lines, line);
    if (closingLine === undefined) continue;
    for (let commentLine = line; commentLine <= closingLine; commentLine++) {
      lines[commentLine] = "";
    }
    line = closingLine;
  }
  return lines.join("\n");
}

function addPath(
  paths: PathReference[],
  kind: PathKind,
  line: number,
  source: string,
  rawPath: string,
): void {
  const path = unquote(rawPath.replace(/^\s*@/, "").trim());
  if (!path || path.includes("{{")) return;
  paths.push({ kind, path, range: valueRange(line, source, path) });
}

export function indexDocument(uri: string, text: string): DocumentIndex {
  const lines = text.split(/\r?\n/);
  const requests: RequestDefinition[] = [];
  const requestReferences: Occurrence[] = [];
  const variables: Occurrence[] = [];
  const variableReferences: VariableReference[] = [];
  const paths: PathReference[] = [];
  const metadata: MetadataEntry[] = [];
  const jsonBodies: DocumentIndex["jsonBodies"] = [];
  const protectedLines = new Set<number>();
  let pendingName: MetadataEntry | undefined;
  let pendingDescription: string | undefined;
  let pendingReferences: Occurrence[] = [];
  let currentRequest: RequestDefinition | undefined;
  let inNativeScript = false;
  let inIntellijScript = false;

  for (let lineNumber = 0; lineNumber < lines.length; lineNumber++) {
    const source = lines[lineNumber];
    const trimmed = source.trim();

    if (inNativeScript) {
      protectedLines.add(lineNumber);
      if (/^\s*}\}\s*$/.test(source)) inNativeScript = false;
      continue;
    }
    if (inIntellijScript) {
      protectedLines.add(lineNumber);
      if (/%}\s*$/.test(source)) inIntellijScript = false;
      continue;
    }
    if (/^\s*<\s*\{%\s*/.test(source) || /^\s*>\s*\{%\s*/.test(source)) {
      inIntellijScript = !/%}\s*$/.test(source);
      protectedLines.add(lineNumber);
      const pathMatch = source.match(/^\s*[<>]\s+(.+\.(?:js|mjs|cjs|ts))\s*$/i);
      if (pathMatch) addPath(paths, "script", lineNumber, source, pathMatch[1]);
      continue;
    }
    if (/^\s*\{\{/.test(source) && !/^\s*\{\{[^}]+}}\s*$/.test(source)) {
      inNativeScript = !/}}\s*$/.test(source);
      protectedLines.add(lineNumber);
      continue;
    }

    if (regionSeparatorPattern.test(source)) {
      if (currentRequest) currentRequest.endLine = Math.max(currentRequest.startLine, lineNumber - 1);
      currentRequest = undefined;
      pendingName = undefined;
      pendingDescription = undefined;
      pendingReferences = [];
      continue;
    }

    const metadataMatch = source.match(metadataPattern);
    if (metadataMatch) {
      const name = metadataMatch[1];
      const value = metadataMatch[2]?.trim();
      const nameStart = source.indexOf(name);
      const entry: MetadataEntry = {
        name,
        range: range(lineNumber, nameStart, nameStart + name.length),
        value,
        valueRange: value ? valueRange(lineNumber, source, value) : undefined,
      };
      metadata.push(entry);
      const lowerName = name.toLowerCase();
      if (lowerName === "name") pendingName = entry;
      if (lowerName === "description") pendingDescription = value;
      if ((lowerName === "ref" || lowerName === "forceref") && value && entry.valueRange) {
        const reference = { name: value, range: entry.valueRange };
        requestReferences.push(reference);
        pendingReferences.push(reference);
      }
      const pathKind = pathMetadata.get(lowerName);
      if (pathKind && value) addPath(paths, pathKind, lineNumber, source, value);
      continue;
    }

    const requestMatch = source.match(requestPattern);
    if (requestMatch && (methodSet.has(requestMatch[1].toUpperCase()) || /^[A-Z-]+$/.test(requestMatch[1]))) {
      if (currentRequest) currentRequest.endLine = Math.max(currentRequest.startLine, lineNumber - 1);
      const method = requestMatch[1];
      const url = requestMatch[2];
      const methodStart = source.indexOf(method);
      const urlStart = source.indexOf(url, methodStart + method.length);
      currentRequest = {
        name: pendingName?.value ?? `${method.toUpperCase()} ${url}`,
        explicitName: Boolean(pendingName?.value),
        responseVariableName: pendingName?.value
          ? responseVariableName(pendingName.value)
          : undefined,
        range: pendingName?.valueRange ?? range(lineNumber, methodStart, methodStart + method.length),
        method,
        methodRange: range(lineNumber, methodStart, methodStart + method.length),
        url,
        urlRange: range(lineNumber, urlStart, urlStart + url.length),
        startLine: lineNumber,
        endLine: lines.length - 1,
        description: pendingDescription,
        references: pendingReferences,
      };
      requests.push(currentRequest);
      pendingName = undefined;
      pendingDescription = undefined;
      pendingReferences = [];
    }

    const variableMatch = source.match(variablePattern);
    if (variableMatch && !requestMatch) {
      const name = variableMatch[1];
      const start = source.indexOf(name);
      variables.push({ name, range: range(lineNumber, start, start + name.length) });
    }

    for (const match of source.matchAll(/\{\{\s*(\$?[A-Za-z_][\w.-]*)/g)) {
      const name = match[1];
      const start = (match.index ?? 0) + match[0].lastIndexOf(name);
      const rootName = name.split(".")[0];
      variableReferences.push({
        name,
        range: range(lineNumber, start, start + name.length),
        rootName,
        rootRange: range(lineNumber, start, start + rootName.length),
      });
    }

    const externalMatch = source.match(/^\s*<\s+(.+?)\s*$/);
    if (externalMatch) {
      const kind = /\.(?:[cm]?js|ts)$/i.test(externalMatch[1]) ? "script" : "body";
      addPath(paths, kind, lineNumber, source, externalMatch[1]);
    }
    const protoMatch = source.match(/^\s*proto\s*<\s*(.+?\.proto)\s*$/i);
    if (protoMatch) addPath(paths, "proto", lineNumber, source, protoMatch[1]);
  }

  if (currentRequest) currentRequest.endLine = lines.length - 1;

  for (const request of requests) {
    let bodyStart = request.startLine + 1;
    while (bodyStart <= request.endLine && lines[bodyStart].trim() !== "") bodyStart++;
    while (bodyStart <= request.endLine && lines[bodyStart].trim() === "") bodyStart++;
    if (bodyStart > request.endLine) continue;
    const hasJsonContentType = lines
      .slice(request.startLine + 1, bodyStart)
      .some((line) => /^\s*Content-Type\s*:\s*application\/(?:[\w.+-]*\+)?json\b/i.test(line));
    const jsonContentStart = hasJsonContentType
      ? findJsonContentStartLine(lines, bodyStart, request.endLine)
      : undefined;
    const hasJsonBody = jsonContentStart !== undefined && /^\s*[\[{]/.test(lines[jsonContentStart]);
    let bodyEnd = hasJsonBody
      ? findJsonBodyEndLine(lines, bodyStart, request.endLine)
      : request.endLine;
    while (bodyEnd >= bodyStart && lines[bodyEnd].trim() === "") bodyEnd--;
    for (let line = bodyStart; line <= bodyEnd; line++) protectedLines.add(line);
    if (!hasJsonBody) continue;
    const bodyText = lines.slice(bodyStart, bodyEnd + 1).join("\n");
    jsonBodies.push({
      range: Range.create(
        Position.create(bodyStart, 0),
        Position.create(bodyEnd, lines[bodyEnd]?.length ?? 0),
      ),
      text: normalizeJsonBodyComments(bodyText),
    });
  }

  return {
    uri,
    text,
    lines,
    requests,
    requestReferences,
    variables,
    variableReferences,
    paths,
    metadata,
    jsonBodies,
    protectedLines,
  };
}

export function responseVariableName(name: string): string {
  return name
    .trim()
    .replace(/\s/g, "-")
    .replace(/-./g, (value) => value[1].toUpperCase());
}

export function wordAt(index: DocumentIndex, position: Position): Occurrence | undefined {
  const candidates: Occurrence[] = [
    ...index.requests,
    ...index.requestReferences,
    ...index.variables,
    ...index.variableReferences,
  ];
  return candidates.find(({ range: candidate }) =>
    candidate.start.line === position.line &&
    candidate.start.character <= position.character &&
    candidate.end.character >= position.character
  );
}
