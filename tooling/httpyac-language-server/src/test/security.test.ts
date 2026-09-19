import assert from "node:assert/strict";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import test from "node:test";
import { loadSafeHttpYacCatalog } from "../security";

test("loads static catalogs without loading the httpyac execution engine", async () => {
  let networkCalls = 0;
  const intercept = (): never => {
    networkCalls++;
    throw new Error("unexpected network access");
  };
  const httpModule = http as unknown as Record<string, unknown>;
  const httpsModule = https as unknown as Record<string, unknown>;
  const netModule = net as unknown as Record<string, unknown>;
  const originals = {
    httpRequest: httpModule.request,
    httpGet: httpModule.get,
    httpsRequest: httpsModule.request,
    httpsGet: httpsModule.get,
    netConnect: netModule.connect,
    netCreateConnection: netModule.createConnection,
    fetch: globalThis.fetch,
  };
  httpModule.request = intercept;
  httpModule.get = intercept;
  httpsModule.request = intercept;
  httpsModule.get = intercept;
  netModule.connect = intercept;
  netModule.createConnection = intercept;
  globalThis.fetch = intercept;

  try {
    process.env.HTTPYAC_PLUGIN = "malicious-project-plugin";
    const catalog = loadSafeHttpYacCatalog();
    assert.equal(process.env.HTTPYAC_PLUGIN, undefined);
    assert(catalog.metadata.some((item) => item.name === "name"));
    assert(catalog.headers.length > 0);
    assert.equal(networkCalls, 0, "loading static catalogs must not access the network");

    assert.equal(
      Object.keys(require.cache).some((name) => /node_modules[\\/]httpyac[\\/]/.test(name)),
      false,
      "httpyac must not be loaded at runtime",
    );
  } finally {
    httpModule.request = originals.httpRequest;
    httpModule.get = originals.httpGet;
    httpsModule.request = originals.httpsRequest;
    httpsModule.get = originals.httpsGet;
    netModule.connect = originals.netConnect;
    netModule.createConnection = originals.netCreateConnection;
    globalThis.fetch = originals.fetch;
  }
});
