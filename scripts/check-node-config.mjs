import assert from "node:assert/strict";

const { normalizeNodeUrl } = await import("../src/services/nodeConfig.ts");

assert.equal(normalizeNodeUrl(" https://viewer.example.com/ "), "https://viewer.example.com");
assert.equal(normalizeNodeUrl("http://192.168.1.20:3000/"), "http://192.168.1.20:3000");
assert.throws(() => normalizeNodeUrl("ftp://viewer.example.com"));
assert.throws(() => normalizeNodeUrl("https://user:pass@viewer.example.com"));
assert.throws(() => normalizeNodeUrl("https://viewer.example.com/subpath"));

console.log("node config check passed");
