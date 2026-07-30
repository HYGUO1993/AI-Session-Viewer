import assert from "node:assert/strict";

process.env.TZ = "Asia/Shanghai";

const {
  formatDateOnly,
  formatDateTime,
  formatShortDateTime,
  normalizeTimeZone,
} = await import("../src/utils/dateTime.ts");

const timestamp = "2026-06-26T13:41:38Z";
assert.equal(formatShortDateTime(timestamp), "06-26 21:41:38");
assert.equal(formatShortDateTime(timestamp, "Asia/Shanghai"), "06-26 21:41:38");
assert.equal(formatDateTime(timestamp, "UTC"), "2026-06-26 13:41:38");
assert.equal(formatDateOnly(timestamp, "Asia/Shanghai"), "2026-06-26");
assert.equal(normalizeTimeZone("Invalid/Zone"), "");

console.log("timezone check passed");
