import { makeServer } from "./server.js";
import { NearAiLlm, RpcChain, nearAiIcon } from "./nearai.js";

const key = process.env.NEAR_AI_API_KEY;
if (!key) throw new Error("NEAR_AI_API_KEY is required (the only secret this service has; it holds no NEAR key)");
const root = process.env.NME_ROOT ?? "nme.testnet";
const port = Number(process.env.PORT ?? 8788);
const server = makeServer({
  llm: new NearAiLlm(key, process.env.LLM_MODEL),
  icon: nearAiIcon(key),
  chain: new RpcChain(process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com"),
  root,
  wrap: process.env.WRAP ?? "wrap.testnet",
  corsOrigin: process.env.CORS_ORIGIN ?? "http://localhost:3000",
  perMin: Number(process.env.RATE_PER_MIN ?? 10),
});
server.listen(port, () => console.log(`nme intent service on :${port} (parse + preview only, no signing), root ${root}`));
