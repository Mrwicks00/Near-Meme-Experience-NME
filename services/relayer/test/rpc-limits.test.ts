import assert from "node:assert/strict";
import { test } from "node:test";
import { RATE_LIMITED } from "../src/chain.js";

test("public RPC rate-limit answers are recognised as retryable", () => {
  assert.ok(RATE_LIMITED.test('{"jsonrpc": "2.0", "error": {"code": -429, "message": "Rate limits exceeded. For higher rate limits get a subscription at https://fastnear.com"}, "id": "1"}'));
  assert.ok(RATE_LIMITED.test("HTTP 429 Too Many Requests"));
  assert.ok(!RATE_LIMITED.test("Smart contract panicked: the deposit was already paid out"));
  assert.ok(!RATE_LIMITED.test("account 4290.testnet does not exist"));
});
