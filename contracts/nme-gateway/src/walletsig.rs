//! Wallet-signed actions (D-57, spikes S7 / S7b): a Solana (Phantom `signMessage`) or EVM (MetaMask `personal_sign`)
//! wallet authorises one action by signing a plain-text message that the contract rebuilds from the call's own
//! arguments. The text names the gateway account, the owner key, the action, a sequential nonce and an expiry, so a
//! signature cannot be replayed on another contract, another action, another owner or later.
use near_sdk::{bs58, env, require, AccountId};

/// A wallet owner key: `sol:<base58 ed25519 pubkey>` or `evm:<0x lowercase 40-hex address>`.
#[derive(Clone, Debug, PartialEq)]
pub enum Owner {
    Sol([u8; 32]),
    Evm([u8; 20]),
}

impl Owner {
    /// Parses and validates; the canonical string (`key()`) is the ledger key, so parsing must be strict.
    pub fn parse(s: &str) -> Result<Owner, String> {
        if let Some(b58) = s.strip_prefix("sol:") {
            let bytes = bs58::decode(b58).into_vec().map_err(|_| "owner: bad base58 public key".to_string())?;
            let pk: [u8; 32] = bytes.try_into().map_err(|_| "owner: a Solana public key is 32 bytes".to_string())?;
            let o = Owner::Sol(pk);
            if o.key() != s {
                return Err("owner: not in canonical form".into());
            }
            return Ok(o);
        }
        if let Some(hex) = s.strip_prefix("evm:0x") {
            if hex.len() != 40 || !hex.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
                return Err("owner: an EVM address is 0x + 40 lowercase hex digits".into());
            }
            let mut a = [0u8; 20];
            for (i, b) in a.iter_mut().enumerate() {
                *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| "owner: bad hex".to_string())?;
            }
            return Ok(Owner::Evm(a));
        }
        Err("owner must be sol:<pubkey> or evm:0x<address>".into())
    }

    pub fn key(&self) -> String {
        match self {
            Owner::Sol(pk) => format!("sol:{}", bs58::encode(pk).into_string()),
            Owner::Evm(a) => format!("evm:0x{}", a.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        }
    }
}

/// The exact text the wallet shows and signs.
pub fn message(gateway: &AccountId, owner_key: &str, action: &str, nonce: u64, expires_ms: u64) -> String {
    format!("NME v1\ngateway: {gateway}\nowner: {owner_key}\naction: {action}\nnonce: {nonce}\nexpires: {expires_ms}")
}

/// EIP-191 `personal_sign` digest.
pub fn eip191_digest(msg: &[u8]) -> [u8; 32] {
    let mut data = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    data.extend_from_slice(msg);
    let h = env::keccak256(&data);
    h.try_into().expect("keccak256 is 32 bytes")
}

/// Checks `sig` is the owner's signature over `msg`. Panics (rejects the call) when it is not.
pub fn verify(owner: &Owner, msg: &[u8], sig: &[u8]) {
    match owner {
        Owner::Sol(pk) => {
            let s: [u8; 64] = sig.try_into().unwrap_or_else(|_| env::panic_str("a Solana signature is 64 bytes"));
            require!(env::ed25519_verify(&s, msg, pk), "bad Solana signature");
        }
        Owner::Evm(addr) => {
            require!(sig.len() == 65, "an EVM signature is 65 bytes (r, s, v)");
            let v = match sig[64] {
                27 | 28 => sig[64] - 27,
                0 | 1 => sig[64],
                _ => env::panic_str("bad EVM recovery id"),
            };
            let digest = eip191_digest(msg);
            // malleability_flag = true: high-s signatures are refused, so a signature has exactly one valid form.
            let pubkey = env::ecrecover(&digest, &sig[..64], v, true).unwrap_or_else(|| env::panic_str("bad EVM signature"));
            let hash = env::keccak256(&pubkey);
            require!(hash[12..] == addr[..], "the signature is from a different address");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    fn gw() -> AccountId {
        "gateway.nme.testnet".parse().unwrap()
    }

    fn sol_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    fn evm_key() -> k256::ecdsa::SigningKey {
        k256::ecdsa::SigningKey::from_bytes((&[9u8; 32]).into()).unwrap()
    }

    fn evm_address(k: &k256::ecdsa::SigningKey) -> String {
        let pk = k.verifying_key().to_encoded_point(false);
        let h = env::keccak256(&pk.as_bytes()[1..]);
        format!("evm:0x{}", h[12..].iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    /// MetaMask `personal_sign`: r || s || v with v = 27 + recovery id.
    fn personal_sign(k: &k256::ecdsa::SigningKey, msg: &[u8]) -> Vec<u8> {
        let digest = eip191_digest(msg);
        let (sig, rec) = k.sign_prehash_recoverable(&digest).unwrap();
        let mut out = sig.to_bytes().to_vec();
        out.push(27 + rec.to_byte());
        out
    }

    fn msg() -> String {
        message(&gw(), "OWNER", "fund_chest:pepe:250000000000000000000000", 3, 1_794_000_000_000)
    }

    #[test]
    fn the_message_is_plain_readable_text_naming_everything() {
        let m = msg();
        assert_eq!(m, "NME v1\ngateway: gateway.nme.testnet\nowner: OWNER\naction: fund_chest:pepe:250000000000000000000000\nnonce: 3\nexpires: 1794000000000");
    }

    #[test]
    fn a_phantom_style_ed25519_signature_over_the_text_verifies_and_nothing_else_does() {
        use ed25519_dalek::Signer;
        testing_env!(VMContextBuilder::new().build());
        let sk = sol_key();
        let owner = Owner::parse(&format!("sol:{}", bs58::encode(sk.verifying_key().to_bytes()).into_string())).unwrap();
        let sig = sk.sign(msg().as_bytes()).to_bytes();
        verify(&owner, msg().as_bytes(), &sig);
        // another nonce, action, gateway or owner: a different text, so the same signature fails
        for other in [
            message(&gw(), "OWNER", "fund_chest:pepe:250000000000000000000000", 4, 1_794_000_000_000),
            message(&gw(), "OWNER", "fund_chest:frog:250000000000000000000000", 3, 1_794_000_000_000),
            message(&"other.testnet".parse().unwrap(), "OWNER", "fund_chest:pepe:250000000000000000000000", 3, 1_794_000_000_000),
            message(&gw(), "OWNER", "fund_chest:pepe:250000000000000000000000", 3, 1_794_000_000_001),
        ] {
            assert!(std::panic::catch_unwind(|| verify(&owner, other.as_bytes(), &sig)).is_err());
        }
        let stranger = Owner::Sol(ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]).verifying_key().to_bytes());
        assert!(std::panic::catch_unwind(|| verify(&stranger, msg().as_bytes(), &sig)).is_err(), "someone else's key");
        assert!(std::panic::catch_unwind(|| verify(&owner, msg().as_bytes(), &sig[..63])).is_err(), "short signature");
    }

    #[test]
    fn a_metamask_personal_sign_signature_recovers_to_the_owner_address() {
        testing_env!(VMContextBuilder::new().build());
        let k = evm_key();
        let owner = Owner::parse(&evm_address(&k)).unwrap();
        let sig = personal_sign(&k, msg().as_bytes());
        verify(&owner, msg().as_bytes(), &sig);
        // wallets that return v = 0/1 are accepted too
        let mut low = sig.clone();
        low[64] -= 27;
        verify(&owner, msg().as_bytes(), &low);
        // a different text, a different address, a bad v, a short signature
        let other = message(&gw(), "OWNER", "fund_chest:pepe:1", 3, 1_794_000_000_000);
        assert!(std::panic::catch_unwind(|| verify(&owner, other.as_bytes(), &sig)).is_err());
        let stranger = Owner::parse("evm:0x0000000000000000000000000000000000000001").unwrap();
        assert!(std::panic::catch_unwind(|| verify(&stranger, msg().as_bytes(), &sig)).is_err());
        let mut bad_v = sig.clone();
        bad_v[64] = 5;
        assert!(std::panic::catch_unwind(|| verify(&owner, msg().as_bytes(), &bad_v)).is_err());
        assert!(std::panic::catch_unwind(|| verify(&owner, msg().as_bytes(), &sig[..64])).is_err());
    }

    #[test]
    fn the_known_ethereum_test_vector_hashes_and_recovers() {
        // Well-known address of the private key 0x...01 is 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf.
        testing_env!(VMContextBuilder::new().build());
        let mut k1 = [0u8; 32];
        k1[31] = 1;
        let k = k256::ecdsa::SigningKey::from_bytes((&k1).into()).unwrap();
        assert_eq!(evm_address(&k), "evm:0x7e5f4552091a69125d5dfcb7b8c2659029395bdf");
        // EIP-191 digest of "hello" (the value every web3 library prints for personal_sign("hello")).
        assert_eq!(hex(&eip191_digest(b"hello")), "50b2c43fd39106bafbba0da34fc430e1f91e3c96ea2acee2bc34119f92b37750");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn owner_keys_must_be_canonical() {
        assert!(Owner::parse("evm:0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf").is_err(), "mixed case");
        assert!(Owner::parse("evm:0x1234").is_err());
        assert!(Owner::parse("evm:7e5f4552091a69125d5dfcb7b8c2659029395bdf").is_err(), "no 0x");
        assert!(Owner::parse("sol:short").is_err());
        assert!(Owner::parse("near:alice.testnet").is_err());
        let sk = sol_key();
        let key = format!("sol:{}", bs58::encode(sk.verifying_key().to_bytes()).into_string());
        assert_eq!(Owner::parse(&key).unwrap().key(), key);
    }
}
