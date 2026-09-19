const fs = require("node:fs");
const path = require("node:path");
delete process.env.HTTPYAC_PLUGIN;
const httpyac = require("httpyac");
const deny = () => { throw new Error("catalog extraction must be static-only"); };
Object.assign(httpyac.io.javascriptProvider, {
  runScript: deny, evalExpression: deny, loadModule: deny, require: Object.freeze({}),
});
Object.assign(httpyac.io.httpClientProvider, { createRequestClient: deny, exchange: deny });
const flatten = (providers) => providers.flatMap((provider) => {
  try { return provider(); } catch { return []; }
});
const catalog = {
  metadata: [...httpyac.utils.knownMetaData],
  emptyLine: flatten(httpyac.io.completionItemProvider.emptyLineProvider),
  variables: flatten(httpyac.io.completionItemProvider.variableProvider),
  headers: httpyac.io.completionItemProvider.requestHeaderProvider.flatMap((provider) => {
    try {
      return provider({ protocol: "HTTP", method: "GET", url: "https://example.invalid", headers: {} });
    } catch { return []; }
  }),
};
const output = JSON.stringify(catalog, null, 2) + "\n";
const destination = path.join(__dirname, "../src/catalog.json");
if (process.argv.includes("--check")) {
  require("node:assert/strict").equal(fs.readFileSync(destination, "utf8"), output);
} else {
  fs.writeFileSync(destination, output);
}
