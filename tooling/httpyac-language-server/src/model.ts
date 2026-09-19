import type { Range } from "vscode-languageserver";

export interface Occurrence {
  name: string;
  range: Range;
}

export interface VariableReference extends Occurrence {
  rootName: string;
  rootRange: Range;
}

export interface RequestDefinition extends Occurrence {
  explicitName: boolean;
  responseVariableName?: string;
  method?: string;
  methodRange?: Range;
  url?: string;
  urlRange?: Range;
  startLine: number;
  endLine: number;
  description?: string;
  references: Occurrence[];
}

export type PathKind = "import" | "body" | "proto" | "script";

export interface PathReference {
  kind: PathKind;
  path: string;
  range: Range;
}

export interface MetadataEntry extends Occurrence {
  value?: string;
  valueRange?: Range;
}

export interface DocumentIndex {
  uri: string;
  text: string;
  lines: string[];
  requests: RequestDefinition[];
  requestReferences: Occurrence[];
  variables: Occurrence[];
  variableReferences: VariableReference[];
  paths: PathReference[];
  metadata: MetadataEntry[];
  jsonBodies: Array<{ range: Range; text: string }>;
  protectedLines: Set<number>;
}
