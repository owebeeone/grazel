const assert = require("node:assert");
assert.strictEqual("gryth".toUpperCase(), "GRYTH");
assert.match("grazel-2026", /^grazel-\d+$/);
console.log("strings: ok");
