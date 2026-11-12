// Dev tool: signs one gateway wallet action with the throwaway Solana test key and sends it through the local relayer.
//   node scripts/wallet-drive.mjs '{"Buy":{"key":"pepe","amount":"1000000000000000000000000","min_out":"1"}}'
// Key file: $NME_WALLET_KEY or ~/.nme-throwaway-wallet.json ({"sk":"<hex ed25519 seed>","owner":"sol:<pubkey>"}). Testnet only; this is NOT a real wallet.
// Needs the relayer on :8787 (services/relayer, npm start). It is a test driver, not part of the product.
import { ed25519 } from "../services/relayer/node_modules/@noble/curves/ed25519.js";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { sk, owner } = JSON.parse(fs.readFileSync(process.env.NME_WALLET_KEY ?? path.join(os.homedir(), ".nme-throwaway-wallet.json"), "utf8"));
const action = JSON.parse(process.argv[2]);
const w = JSON.parse(execFileSync("python3", [path.join(root, "scripts/lib/nview.py"), "gateway.nme.testnet", "get_wallet", JSON.stringify({ owner })], { cwd: root }).toString());
const nonce = w.nonce, expires_ms = Date.now() + 120000;
const post = async (p, b) => { for (let i = 0; i < 3; i++) { try { const r = await fetch("http://localhost:8787" + p, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(b), signal: AbortSignal.timeout(300000) }); return [r.status, await r.json()]; } catch { await new Promise((r) => setTimeout(r, 1500)); } } throw new Error("relayer unreachable"); };
const [ms, m] = await post("/message", { owner, action, nonce, expires_ms });
if (ms !== 200) { console.log("message", ms, JSON.stringify(m).slice(0, 200)); process.exit(1); }
const signature = Buffer.from(ed25519.sign(Buffer.from(m.message ?? m.text ?? m), Buffer.from(sk, "hex"))).toString("base64");
const [s, b] = await post("/action", { owner, action, nonce, expires_ms, signature });
console.log(s, JSON.stringify(b).slice(0, 300));
