import { NearChain } from "./chain.js";
import { loadConfig } from "./config.js";
import { makePrecheck } from "./precheck.js";
import { Relayer } from "./relayer.js";
import { makeServer } from "./server.js";

const cfg = loadConfig();
const relayer = new Relayer(new NearChain(cfg.rpc, cfg.account, cfg.key), cfg, makePrecheck(cfg.solanaRpc, cfg.evmRpc));
relayer.startRefundSweep();
makeServer(relayer, cfg).listen(cfg.port, () => console.log(`nme relayer on :${cfg.port} as ${cfg.account} -> ${cfg.gateway}`));
