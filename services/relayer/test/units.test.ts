import assert from "node:assert/strict";
import { test } from "node:test";
import { RateLimit } from "../src/ratelimit.js";
import { verifyOwner } from "../src/sigcheck.js";
import { BadRequest, parseAction, parseOwner } from "../src/validate.js";
import { EVM_OWNER, SOL_OWNER, signText } from "./helpers.js";

const bad = (f: () => unknown, re: RegExp) => assert.throws(f, (e: unknown) => e instanceof BadRequest && re.test(e.message));

test("owners: only canonical sol: and lowercase evm: keys", () => {
  assert.equal(parseOwner(SOL_OWNER), SOL_OWNER);
  assert.equal(parseOwner(EVM_OWNER), EVM_OWNER);
  for (const o of ["alice.testnet", "evm:0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD", "evm:0x12", "sol:0OIl", "", 5]) bad(() => parseOwner(o), /owner/);
});

test("actions: every variant parses; strangers, extras, zero and separators are refused", () => {
  const good = [
    { FundChest: { ticker: "pepe", amount: "5" } },
    { TransferNear: { to: "bob.testnet", amount: "5" } },
    { Hire: { ticker: "pepe", kind: "roast", target: "BATMAN", prompt: "be kind", amount: "5" } },
    { Buy: { key: "pepe", amount: "5", min_out: "1" } },
    { JoinWar: { key: "pepe", amount: "5", min_out: "1" } },
    { Sell: { token: "pepe.nme.testnet", amount: "5", min_out: "1" } },
    { TransferTokens: { token: "pepe.nme.testnet", to: "bob.testnet", amount: "5" } },
  ];
  for (const a of good) assert.deepEqual(parseAction(a), a);
  bad(() => parseAction({ Mint: { amount: "1" } }), /unknown action/);
  bad(() => parseAction({ Buy: { key: "pepe", amount: "5", min_out: "1" }, Sell: {} }), /exactly one/);
  bad(() => parseAction({ Buy: { key: "pepe", amount: "5", min_out: "1", extra: 1 } }), /unexpected field/);
  bad(() => parseAction({ Buy: { key: "pe:pe", amount: "5", min_out: "1" } }), /key/);
  bad(() => parseAction({ Buy: { key: "pe\npe", amount: "5", min_out: "1" } }), /key/);
  bad(() => parseAction({ Buy: { key: "pepe", amount: "0", min_out: "1" } }), /above zero/);
  bad(() => parseAction({ Buy: { key: "pepe", amount: "-5", min_out: "1" } }), /amount/);
  bad(() => parseAction({ Buy: { key: "pepe", amount: 5, min_out: "1" } }), /amount/);
  bad(() => parseAction({ Sell: { token: "NOT AN ACCOUNT", amount: "5", min_out: "1" } }), /token/);
  bad(() => parseAction({ Hire: { ticker: "pepe", kind: "roast", target: "x", prompt: "x".repeat(4001), amount: "5" } }), /prompt/);
});

test("signatures: Phantom (ed25519) and MetaMask (EIP-191) verify; anything else does not", () => {
  const text = "NME v1\ngateway: gateway.nme.testnet\nowner: x\naction: buy:pepe:5:1\nnonce: 0\nexpires: 1";
  for (const owner of [SOL_OWNER, EVM_OWNER]) {
    const sig = Buffer.from(signText(owner, text), "base64");
    assert.equal(verifyOwner(owner, text, sig), true, owner);
    assert.equal(verifyOwner(owner, text + " ", sig), false, "another text");
    const flipped = Buffer.from(sig);
    flipped[3] = flipped[3]! ^ 1;
    assert.equal(verifyOwner(owner, text, flipped), false, "a flipped bit");
    assert.equal(verifyOwner(owner, text, sig.subarray(0, 60)), false, "a short signature");
  }
  const solSig = Buffer.from(signText(SOL_OWNER, text), "base64");
  const evmSig = Buffer.from(signText(EVM_OWNER, text), "base64");
  assert.equal(verifyOwner(EVM_OWNER, text, solSig), false, "a Solana signature is not an EVM one");
  assert.equal(verifyOwner(SOL_OWNER, text, evmSig), false, "an EVM signature is not a Solana one");
  assert.equal(verifyOwner("evm:0x0000000000000000000000000000000000000001", text, evmSig), false, "another address");
});

test("rate limit: a sliding window per key, refused hits are not recorded", () => {
  let t = 0;
  const rl = new RateLimit(2, 1000, () => t);
  assert.ok(rl.take("a") && rl.take("a"));
  assert.equal(rl.take("a"), false);
  assert.ok(rl.take("b"), "another key is separate");
  assert.equal(rl.retryAfter("a"), 1);
  t = 1001;
  assert.ok(rl.take("a"), "the window moved on");
});
