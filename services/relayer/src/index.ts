import { NearChain } from "./chain.js";
import { loadConfig } from "./config.js";
import { makePrecheck } from "./precheck.js";
import { Relayer } from "./relayer.js";
import { makeServer } from "./server.js";
import { GoogleVerifier, NearPlayChain, Play, PlayStore } from "./play.js";

const cfg = loadConfig();
const relayer = new Relayer(new NearChain(cfg.rpc, cfg.account, cfg.key), cfg, makePrecheck(cfg.solanaRpc, cfg.evmRpc));
relayer.startRefundSweep();
// Google sign-in accounts (optional: needs GOOGLE_CLIENT_ID, PLAY_PARENT_KEY and PLAY_RECOVERY_KEY).
const play =
  process.env.GOOGLE_CLIENT_ID && process.env.PLAY_PARENT_KEY && process.env.PLAY_RECOVERY_KEY
    ? new Play(
        new GoogleVerifier(process.env.GOOGLE_CLIENT_ID),
        new PlayStore(process.env.PLAY_STORE ?? "data/play-accounts.json"),
        new NearPlayChain(cfg.rpc, process.env.PLAY_PARENT ?? "nmeplay.testnet", process.env.PLAY_PARENT_KEY, process.env.PLAY_RECOVERY_KEY),
        { parent: process.env.PLAY_PARENT ?? "nmeplay.testnet", starterYocto: BigInt(process.env.PLAY_STARTER_YOCTO ?? "500000000000000000000000"), signupsPerIpPerDay: Number(process.env.PLAY_SIGNUPS_PER_IP ?? 3) },
      )
    : undefined;
if (!play) console.log("Google sign-in accounts: disabled (set GOOGLE_CLIENT_ID, PLAY_PARENT_KEY, PLAY_RECOVERY_KEY)");
makeServer(relayer, cfg, play).listen(cfg.port, () => console.log(`nme relayer on :${cfg.port} as ${cfg.account} -> ${cfg.gateway}`));
