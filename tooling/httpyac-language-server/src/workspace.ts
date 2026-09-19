import { promises as fs } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { Location } from "vscode-languageserver";
import { indexDocument } from "./indexer";
import type { DocumentIndex, Occurrence, PathReference } from "./model";

const HTTP_EXTENSIONS = new Set([".http", ".rest"]);

export function uriToPath(uri: string): string | undefined {
  try {
    return uri.startsWith("file:") ? fileURLToPath(uri) : undefined;
  } catch {
    return undefined;
  }
}

export function resolvePath(uri: string, reference: string): string | undefined {
  const fileName = uriToPath(uri);
  if (!fileName || /^(?:https?|wss?|grpc):\/\//i.test(reference)) return undefined;
  const normalized = reference.replace(/^~(?=[\\/])/, process.env.USERPROFILE ?? "~");
  return path.resolve(path.dirname(fileName), normalized);
}

export class WorkspaceIndex {
  private readonly documents = new Map<string, DocumentIndex>();
  private readonly roots = new Set<string>();

  setRoots(roots: string[]): void {
    this.roots.clear();
    for (const root of roots) {
      const fileName = uriToPath(root);
      if (fileName) this.roots.add(fileName);
    }
  }

  update(uri: string, text: string): DocumentIndex {
    const indexed = indexDocument(uri, text);
    this.documents.set(uri, indexed);
    return indexed;
  }

  remove(uri: string): void {
    this.documents.delete(uri);
  }

  get(uri: string): DocumentIndex | undefined {
    return this.documents.get(uri);
  }

  all(): DocumentIndex[] {
    return [...this.documents.values()];
  }

  async loadFile(fileName: string): Promise<DocumentIndex | undefined> {
    const uri = pathToFileURL(fileName).toString();
    const existing = this.documents.get(uri);
    if (existing) return existing;
    try {
      const text = await fs.readFile(fileName, "utf8");
      return this.update(uri, text);
    } catch {
      return undefined;
    }
  }

  async loadImports(index: DocumentIndex, seen = new Set<string>()): Promise<void> {
    if (seen.has(index.uri)) return;
    seen.add(index.uri);
    for (const reference of index.paths.filter((item) => item.kind === "import")) {
      const fileName = resolvePath(index.uri, reference.path);
      if (!fileName) continue;
      const imported = await this.loadFile(fileName);
      if (imported) await this.loadImports(imported, seen);
    }
  }

  async scan(): Promise<void> {
    for (const root of this.roots) await this.scanDirectory(root);
  }

  private async scanDirectory(directory: string): Promise<void> {
    let entries;
    try {
      entries = await fs.readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.name === "node_modules" || entry.name === ".git" || entry.name === "target") continue;
      const fileName = path.join(directory, entry.name);
      if (entry.isDirectory()) await this.scanDirectory(fileName);
      else if (HTTP_EXTENSIONS.has(path.extname(entry.name).toLowerCase())) await this.loadFile(fileName);
    }
  }

  async reachable(index: DocumentIndex): Promise<DocumentIndex[]> {
    await this.loadImports(index);
    const result: DocumentIndex[] = [];
    const visit = async (document: DocumentIndex): Promise<void> => {
      if (result.some((item) => item.uri === document.uri)) return;
      result.push(document);
      for (const reference of document.paths.filter((item) => item.kind === "import")) {
        const fileName = resolvePath(document.uri, reference.path);
        if (!fileName) continue;
        const imported = await this.loadFile(fileName);
        if (imported) await visit(imported);
      }
    };
    await visit(index);
    return result;
  }

  async requestDefinitions(index: DocumentIndex, name: string): Promise<Location[]> {
    const documents = await this.reachable(index);
    return documents.flatMap((document) =>
      document.requests
        .filter((request) => request.name === name)
        .map((request) => ({ uri: document.uri, range: request.range }))
    );
  }

  async variableDefinitions(index: DocumentIndex, name: string): Promise<Location[]> {
    const documents = await this.reachable(index);
    const rootName = name.split(".")[0];
    return documents.flatMap((document) => [
      ...document.variables
        .filter((variable) => variable.name === name || variable.name === rootName)
        .map((variable) => ({ uri: document.uri, range: variable.range })),
      ...document.requests
        .filter((request) => request.responseVariableName === rootName)
        .map((request) => ({ uri: document.uri, range: request.range })),
    ]);
  }

  requestOccurrences(name: string): Array<{ uri: string; occurrence: Occurrence }> {
    return this.all().flatMap((document) => [
      ...document.requests
        .filter((request) => request.name === name)
        .map((occurrence) => ({ uri: document.uri, occurrence })),
      ...document.requestReferences
        .filter((reference) => reference.name === name)
        .map((occurrence) => ({ uri: document.uri, occurrence })),
    ]);
  }

  variableOccurrences(name: string): Array<{ uri: string; occurrence: Occurrence }> {
    return this.all().flatMap((document) => [
      ...document.variables
        .filter((variable) => variable.name === name)
        .map((occurrence) => ({ uri: document.uri, occurrence })),
      ...document.variableReferences
        .filter((reference) =>
          reference.name === name || reference.name.startsWith(`${name}.`)
        )
        .map((reference) => ({
          uri: document.uri,
          occurrence: {
            name,
            range: {
              start: reference.range.start,
              end: {
                line: reference.range.start.line,
                character: reference.range.start.character + name.length,
              },
            },
          },
        })),
    ]);
  }

  responseVariableOccurrences(name: string): Array<{ uri: string; occurrence: Occurrence }> {
    return this.all().flatMap((document) =>
      document.variableReferences
        .filter((reference) => reference.rootName === name)
        .map((reference) => ({
          uri: document.uri,
          occurrence: { name, range: reference.rootRange },
        }))
    );
  }

  hasRequest(name: string): boolean {
    return this.all().some((document) =>
      document.requests.some((request) => request.name === name)
    );
  }

  requestNameForResponseVariable(variableName: string): string | undefined {
    return this.all()
      .flatMap((document) => document.requests)
      .find((request) => request.responseVariableName === variableName)
      ?.name;
  }

  responseVariableForRequest(name: string): string | undefined {
    return this.all()
      .flatMap((document) => document.requests)
      .find((request) => request.name === name)
      ?.responseVariableName;
  }

  findPath(index: DocumentIndex, line: number, character: number): PathReference | undefined {
    return index.paths.find(({ range }) =>
      range.start.line === line &&
      range.start.character <= character &&
      range.end.character >= character
    );
  }
}
