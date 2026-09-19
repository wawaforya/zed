import catalog from "./catalog.json";

export interface HttpYacStaticCatalog {
  metadata: Array<{ name: string; description?: string; completions?: string[] }>;
  emptyLine: Array<{ name: string; description: string; text?: string }>;
  variables: Array<{ name: string; description: string; text?: string }>;
  headers: Array<{ name: string; description: string; text?: string }>;
}

export function loadSafeHttpYacCatalog(): HttpYacStaticCatalog {
  // Never load httpyac or project modules in the language server. Catalogs are
  // extracted from a pinned httpyac version during development, not at startup.
  delete process.env.HTTPYAC_PLUGIN;
  return structuredClone(catalog);
}
