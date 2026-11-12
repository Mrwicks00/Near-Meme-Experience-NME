//! Pure logic of the deposit verifier: reads a Solana `getTransaction` (jsonParsed) result, finds the SOL that reached
//! the expected deposit address, and prices it in NEAR from two independent sources that must agree.
use serde_json::Value;

pub const MAX_PRICE_SPREAD_BPS: u64 = 200; // the two sources may differ by at most 2%

#[derive(Debug, PartialEq)]
pub struct Deposit {
    pub lamports: u64,
    pub sender: String,
    pub slot: u64,
    pub block_time: u64,
}

/// Total lamports sent to `expected_to` by plain system transfers of a *successful* transaction.
pub fn parse_deposit(tx: &Value, expected_to: &str) -> Result<Deposit, String> {
    if tx.is_null() {
        return Err("transaction not found (or not finalized yet)".into());
    }
    if !tx["meta"]["err"].is_null() {
        return Err("the transaction failed on Solana".into());
    }
    let mut lamports: u64 = 0;
    let mut sender = String::new();
    for ins in tx["transaction"]["message"]["instructions"].as_array().into_iter().flatten() {
        if ins["program"] == "system" && ins["parsed"]["type"] == "transfer" {
            let info = &ins["parsed"]["info"];
            if info["destination"] == expected_to {
                lamports = lamports.checked_add(info["lamports"].as_u64().ok_or("bad lamports")?).ok_or("overflow")?;
                if sender.is_empty() {
                    sender = info["source"].as_str().unwrap_or("").to_string();
                }
            }
        }
    }
    if lamports == 0 {
        return Err("no SOL transfer to the expected deposit address in this transaction".into());
    }
    Ok(Deposit { lamports, sender, slot: tx["slot"].as_u64().unwrap_or(0), block_time: tx["blockTime"].as_u64().unwrap_or(0) })
}

/// USD price in micro-dollars from a Coinpaprika ticker: quotes.USD.price.
pub fn coinpaprika(v: &Value) -> Result<u64, String> {
    micro(v["quotes"]["USD"]["price"].as_f64().ok_or("coinpaprika: no price")?)
}

/// Bitstamp ticker: {"last": "88.928"}.
pub fn bitstamp(v: &Value) -> Result<u64, String> {
    micro(v["last"].as_str().and_then(|x| x.parse().ok()).ok_or("bitstamp: no price")?)
}

/// Gate.io tickers: [{"last": "3.665"}].
pub fn gateio(v: &Value) -> Result<u64, String> {
    micro(v[0]["last"].as_str().and_then(|x| x.parse().ok()).ok_or("gateio: no price")?)
}

/// Where each asset's price can come from; the worker takes the first two that answer and requires them to agree.
pub fn sources(asset: &str) -> Vec<(&'static str, String)> {
    let (paprika, bitstamp_pair, gate_pair) = match asset {
        "sol" => ("sol-solana", "solusd", "SOL_USDT"),
        "eth" => ("eth-ethereum", "ethusd", "ETH_USDT"),
        _ => ("near-near-protocol", "nearusd", "NEAR_USDT"),
    };
    vec![
        ("coinpaprika", format!("https://api.coinpaprika.com/v1/tickers/{paprika}")),
        ("bitstamp", format!("https://www.bitstamp.net/api/v2/ticker/{bitstamp_pair}/")),
        ("gateio", format!("https://api.gateio.ws/api/v4/spot/tickers?currency_pair={gate_pair}")),
    ]
}

pub fn parse_source(name: &str, v: &Value) -> Result<u64, String> {
    match name {
        "coinpaprika" => coinpaprika(v),
        "bitstamp" => bitstamp(v),
        "gateio" => gateio(v),
        _ => Err("unknown source".into()),
    }
}

fn micro(p: f64) -> Result<u64, String> {
    if !(p.is_finite() && p > 0.0 && p < 1.0e9) {
        return Err("price out of range".into());
    }
    Ok((p * 1_000_000.0).round() as u64)
}

/// The two sources must agree within `MAX_PRICE_SPREAD_BPS`; the result is their average.
pub fn agree(a: u64, b: u64) -> Result<u64, String> {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if lo == 0 || (hi - lo) as u128 * 10_000 > lo as u128 * MAX_PRICE_SPREAD_BPS as u128 {
        return Err(format!("the two price sources disagree ({a} vs {b} micro-USD)"));
    }
    Ok(((a as u128 + b as u128) / 2) as u64)
}

/// yoctoNEAR worth `lamports` of SOL: lamports/1e9 SOL x sol_usd / near_usd x 1e24 = lamports x sol x 1e15 / near.
pub fn near_yocto(lamports: u64, sol_micro: u64, near_micro: u64) -> Result<u128, String> {
    if near_micro == 0 {
        return Err("zero NEAR price".into());
    }
    (lamports as u128)
        .checked_mul(sol_micro as u128)
        .and_then(|x| x.checked_mul(1_000_000_000_000_000))
        .map(|x| x / near_micro as u128)
        .ok_or_else(|| "overflow".into())
}

/// Ethereum Sepolia (the EVM testnet of the core; chain id 11155111).
pub const SEPOLIA_CHAIN_ID: u64 = 11_155_111;

#[derive(Debug, PartialEq)]
pub struct EvmDeposit {
    pub wei: u128,
    pub sender: String,
    pub block: u64,
}

fn hex_u128(v: &Value, what: &str) -> Result<u128, String> {
    let s = v.as_str().and_then(|s| s.strip_prefix("0x")).ok_or_else(|| format!("{what}: not a hex quantity"))?;
    u128::from_str_radix(s, 16).map_err(|_| format!("{what}: not a hex quantity"))
}

/// A finalized, successful, plain ETH transfer (no calldata) straight to `expected_to`. Contract calls and internal transfers do
/// not count: only the transaction's own `value` to its own `to` does.
pub fn parse_evm_deposit(tx: &Value, receipt: &Value, finalized_block: u64, expected_to: &str) -> Result<EvmDeposit, String> {
    if tx.is_null() {
        return Err("transaction not found".into());
    }
    if receipt.is_null() {
        return Err("transaction is not mined yet".into());
    }
    if receipt["status"] != "0x1" {
        return Err("the transaction failed on Ethereum".into());
    }
    if let Some(c) = tx.get("chainId").filter(|c| !c.is_null()) {
        if hex_u128(c, "chainId")? != SEPOLIA_CHAIN_ID as u128 {
            return Err("the transaction is not on Ethereum Sepolia".into());
        }
    }
    let to = tx["to"].as_str().ok_or("a contract creation is not a deposit")?;
    if !to.eq_ignore_ascii_case(expected_to) {
        return Err("the transaction was not sent to the expected deposit address".into());
    }
    if tx["input"] != "0x" {
        return Err("only plain ETH transfers count (the transaction carries calldata)".into());
    }
    let block = hex_u128(&receipt["blockNumber"], "blockNumber")? as u64;
    if block > finalized_block {
        return Err("the transaction is not finalized yet".into());
    }
    let wei = hex_u128(&tx["value"], "value")?;
    if wei == 0 {
        return Err("the transfer carries no ETH".into());
    }
    Ok(EvmDeposit { wei, sender: tx["from"].as_str().unwrap_or("").to_string(), block })
}

/// yoctoNEAR worth `wei` of ETH: wei/1e18 ETH x eth_usd / near_usd x 1e24 = wei x eth x 1e6 / near.
pub fn wei_to_near_yocto(wei: u128, eth_micro: u64, near_micro: u64) -> Result<u128, String> {
    if near_micro == 0 {
        return Err("zero NEAR price".into());
    }
    wei.checked_mul(eth_micro as u128).and_then(|x| x.checked_mul(1_000_000)).map(|x| x / near_micro as u128).ok_or_else(|| "overflow (deposit too large)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        // a real finalized devnet transaction: 5 SOL from the faucet to ou1oh...
        let v: Value = serde_json::from_str(include_str!("sample_tx.json")).unwrap();
        v["result"].clone()
    }

    const TO: &str = "ou1ohPLwTBPQtUr8kX3N5MWKWzrbJUNMqBz3FTe2kmK";

    #[test]
    fn a_real_devnet_transfer_parses() {
        let d = parse_deposit(&sample(), TO).unwrap();
        assert_eq!((d.lamports, d.sender.as_str()), (5_000_000_000, "devwuNsNYACyiEYxRNqMNseBpNnGfnd4ZwNHL7sphqv"));
        assert!(d.slot > 0 && d.block_time > 0);
    }

    #[test]
    fn a_transfer_to_another_address_is_not_a_deposit() {
        assert!(parse_deposit(&sample(), "11111111111111111111111111111112").unwrap_err().contains("no SOL transfer"));
    }

    #[test]
    fn missing_or_failed_transactions_are_rejected() {
        assert!(parse_deposit(&Value::Null, TO).unwrap_err().contains("not found"));
        let mut t = sample();
        t["meta"]["err"] = json!({"InstructionError": [0, "Custom"]});
        assert!(parse_deposit(&t, TO).unwrap_err().contains("failed"));
    }

    #[test]
    fn several_transfers_to_the_address_add_up() {
        let mut t = sample();
        let ins = t["transaction"]["message"]["instructions"].as_array().unwrap().clone();
        let mut all = ins.clone();
        all.extend(ins);
        t["transaction"]["message"]["instructions"] = json!(all);
        assert_eq!(parse_deposit(&t, TO).unwrap().lamports, 10_000_000_000);
    }

    #[test]
    fn prices_parse_and_must_agree() {
        assert_eq!(bitstamp(&json!({"last": "88.928"})).unwrap(), 88_928_000);
        assert_eq!(gateio(&json!([{"last": "3.665"}])).unwrap(), 3_665_000);
        assert_eq!(coinpaprika(&json!({"quotes": {"USD": {"price": 3.66}}})).unwrap(), 3_660_000);
        assert!(bitstamp(&json!({})).is_err() && gateio(&json!([])).is_err() && coinpaprika(&json!({})).is_err());
        assert_eq!(sources("sol").len(), 3);
        assert!(sources("near")[1].1.contains("nearusd"));
        assert_eq!(parse_source("bitstamp", &json!({"last": "1.5"})).unwrap(), 1_500_000);
        assert!(parse_source("nope", &json!({})).is_err());
        assert!(micro(-1.0).is_err() && micro(f64::NAN).is_err());
        assert_eq!(agree(100_000_000, 101_000_000).unwrap(), 100_500_000);
        assert!(agree(100_000_000, 103_000_000).is_err(), "3% apart: rejected");
        assert!(agree(0, 5).is_err());
    }

    fn evm_sample() -> (Value, Value, u64) {
        let v: Value = serde_json::from_str(include_str!("sample_evm.json")).unwrap();
        (v["tx"].clone(), v["receipt"].clone(), v["finalized_number"].as_u64().unwrap())
    }
    const EVM_TO: &str = "0x022bc100f8e4b973952247463f1b2c980849f88e";

    #[test]
    fn a_real_sepolia_transfer_parses() {
        let (tx, rc, fin) = evm_sample();
        let d = parse_evm_deposit(&tx, &rc, fin, EVM_TO).unwrap();
        assert_eq!((d.wei, d.sender.as_str(), d.block), (4_000_000_000_000_000, "0xca7ca0715cada94feed43eb2f8baa13fe35266d4", 0xb87dd3));
        // checksum-cased expected address matches too
        assert!(parse_evm_deposit(&tx, &rc, fin, &EVM_TO.to_uppercase().replace("0X", "0x")).is_ok());
    }

    #[test]
    fn evm_deposits_must_be_final_successful_plain_transfers_to_the_address() {
        let (tx, rc, fin) = evm_sample();
        let err = |tx: &Value, rc: &Value, f: u64, to: &str| parse_evm_deposit(tx, rc, f, to).unwrap_err();
        assert!(err(&tx, &rc, fin, "0x0000000000000000000000000000000000000001").contains("not sent to the expected"));
        assert!(err(&tx, &rc, 0xb87dd2, EVM_TO).contains("not finalized"), "one block short of final");
        assert!(err(&Value::Null, &rc, fin, EVM_TO).contains("not found"));
        assert!(err(&tx, &Value::Null, fin, EVM_TO).contains("not mined"));
        let mut failed = rc.clone();
        failed["status"] = json!("0x0");
        assert!(err(&tx, &failed, fin, EVM_TO).contains("failed"));
        let mut call = tx.clone();
        call["input"] = json!("0xa9059cbb");
        assert!(err(&call, &rc, fin, EVM_TO).contains("calldata"));
        let mut mainnet = tx.clone();
        mainnet["chainId"] = json!("0x1");
        assert!(err(&mainnet, &rc, fin, EVM_TO).contains("not on Ethereum Sepolia"));
        let mut zero = tx.clone();
        zero["value"] = json!("0x0");
        assert!(err(&zero, &rc, fin, EVM_TO).contains("no ETH"));
        let mut create = tx.clone();
        create["to"] = Value::Null;
        assert!(err(&create, &rc, fin, EVM_TO).contains("contract creation"));
        let mut bad = tx.clone();
        bad["value"] = json!("12");
        assert!(err(&bad, &rc, fin, EVM_TO).contains("hex"));
    }

    #[test]
    fn wei_convert_to_yocto_near() {
        // 1 ETH at $3000 with NEAR at $3.66 = 819.67... NEAR
        let y = wei_to_near_yocto(1_000_000_000_000_000_000, 3_000_000_000, 3_660_000).unwrap();
        assert!(y > 819_600_000_000_000_000_000_000_000 && y < 819_700_000_000_000_000_000_000_000, "{y}");
        assert!(wei_to_near_yocto(1, 1, 0).is_err());
        assert!(wei_to_near_yocto(u128::MAX, 2, 1).is_err());
        assert_eq!(sources("eth").len(), 3);
        assert!(sources("eth")[0].1.contains("eth-ethereum") && sources("eth")[1].1.contains("ethusd") && sources("eth")[2].1.contains("ETH_USDT"));
    }

    #[test]
    fn lamports_convert_to_yocto_near() {
        // 1 SOL at $88.97 with NEAR at $3.66 = 24.3087... NEAR
        let y = near_yocto(1_000_000_000, 88_970_000, 3_660_000).unwrap();
        assert!(y > 24_300_000_000_000_000_000_000_000 && y < 24_310_000_000_000_000_000_000_000, "{y}");
        assert_eq!(near_yocto(0, 1, 1).unwrap(), 0);
        assert!(near_yocto(1, 1, 0).is_err());
    }
}
