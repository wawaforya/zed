import {
  CodeActionParams,
  CompletionParams,
  createConnection,
  DefinitionParams,
  DidChangeConfigurationNotification,
  DocumentFormattingParams,
  DocumentLinkParams,
  HoverParams,
  InitializeParams,
  InitializeResult,
  ProposedFeatures,
  ReferenceParams,
  RenameParams,
  TextDocumentPositionParams,
  TextDocuments,
  TextEdit,
  TextDocumentSyncKind,
  WorkspaceSymbolParams,
} from "vscode-languageserver/node";
import { TextDocument } from "vscode-languageserver-textdocument";
import {
  codeActions,
  catalogVariableNames,
  completions,
  definition,
  diagnostics,
  documentLinks,
  formattingEdits,
  hover,
  references,
  rename,
  workspaceSymbols,
} from "./features";
import { loadSafeHttpYacCatalog } from "./security";
import { WorkspaceIndex } from "./workspace";

const connection = createConnection(ProposedFeatures.all);
const documents = new TextDocuments(TextDocument);
const workspace = new WorkspaceIndex();
const catalog = loadSafeHttpYacCatalog();
const knownMetadata = new Set(catalog.metadata.map((item) => item.name.toLowerCase()));
const knownVariables = catalogVariableNames(catalog);

connection.onInitialize((params: InitializeParams): InitializeResult => {
  const roots = params.workspaceFolders?.map((folder) => folder.uri) ??
    (params.rootUri ? [params.rootUri] : []);
  workspace.setRoots(roots);
  void workspace.scan();
  return {
    serverInfo: { name: "httpyac-language-server", version: "0.4.0" },
    capabilities: {
      textDocumentSync: TextDocumentSyncKind.Incremental,
      completionProvider: {
        triggerCharacters: ["#", "@", "{", "<", "/", "\\", ":"],
      },
      definitionProvider: true,
      referencesProvider: true,
      renameProvider: { prepareProvider: true },
      documentLinkProvider: { resolveProvider: false },
      hoverProvider: true,
      codeActionProvider: true,
      documentFormattingProvider: true,
      workspaceSymbolProvider: true,
    },
  };
});

connection.onInitialized(() => {
  void connection.client.register(DidChangeConfigurationNotification.type);
});

documents.onDidOpen(({ document }) => {
  const index = workspace.update(document.uri, document.getText());
  void publishDiagnostics(index.uri);
});

documents.onDidChangeContent(({ document }) => {
  const index = workspace.update(document.uri, document.getText());
  void publishDiagnostics(index.uri);
});

documents.onDidClose(({ document }) => {
  workspace.remove(document.uri);
  connection.sendDiagnostics({ uri: document.uri, diagnostics: [] });
});

connection.onCompletion(async (params: CompletionParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? completions(workspace, index, params.position, catalog) : [];
});

connection.onDefinition(async (params: DefinitionParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? definition(workspace, index, params.position) : [];
});

connection.onReferences((params: ReferenceParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? references(workspace, index, params.position) : [];
});

connection.onPrepareRename((params: TextDocumentPositionParams) => {
  const index = getIndex(params.textDocument.uri);
  if (!index) return null;
  const variableReference = index.variableReferences.find((item) =>
    item.rootRange.start.line === params.position.line &&
    item.rootRange.start.character <= params.position.character &&
    item.rootRange.end.character >= params.position.character
  );
  if (variableReference) return variableReference.rootRange;
  const occurrence = [
    ...index.requests,
    ...index.requestReferences,
    ...index.variables,
  ].find((item) =>
    item.range.start.line === params.position.line &&
    item.range.start.character <= params.position.character &&
    item.range.end.character >= params.position.character
  );
  return occurrence?.range ?? null;
});

connection.onRenameRequest((params: RenameParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? rename(workspace, index, params.position, params.newName) : null;
});

connection.onDocumentLinks((params: DocumentLinkParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? documentLinks(index) : [];
});

connection.onWorkspaceSymbol((params: WorkspaceSymbolParams) =>
  workspaceSymbols(workspace, params.query)
);

connection.onHover((params: HoverParams) => {
  const index = getIndex(params.textDocument.uri);
  return index ? hover(index, params.position, catalog) : null;
});

connection.onCodeAction(async (params: CodeActionParams) => {
  const index = getIndex(params.textDocument.uri);
  if (!index) return [];
  return codeActions(index, params.context.diagnostics, params.range);
});

connection.onDocumentFormatting((params: DocumentFormattingParams): TextEdit[] => {
  const index = getIndex(params.textDocument.uri);
  return index ? formattingEdits(index) : [];
});

async function publishDiagnostics(uri: string): Promise<void> {
  const index = workspace.get(uri);
  if (!index) return;
  await workspace.loadImports(index);
  connection.sendDiagnostics({
    uri,
    diagnostics: await diagnostics(workspace, index, knownMetadata, knownVariables),
  });
}

function getIndex(uri: string) {
  const current = workspace.get(uri);
  if (current) return current;
  const document = documents.get(uri);
  return document ? workspace.update(uri, document.getText()) : undefined;
}

documents.listen(connection);
connection.listen();
