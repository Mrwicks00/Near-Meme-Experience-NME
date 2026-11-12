//! Pure logic of the Operator: build and sign one NEAR transaction that calls `arena.tick`, nothing else.
//! The worker can only ever produce this one transaction shape (fixed receiver/method/deposit); on top of that the
//! key itself is a function-call key restricted on-chain to `<arena>.tick`, so even a bug here cannot move value.
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

pub const METHOD: &str = "tick";
pub const GAS: u64 = 300_000_000_000_000;

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// Borsh of a `Transaction` (V0 layout) with exactly one FunctionCall action and zero deposit.
pub fn tx_bytes(signer: &str, pubkey: &[u8; 32], nonce: u64, receiver: &str, block_hash: &[u8; 32], args: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    put_str(&mut o, signer);
    o.push(0); // ed25519
    o.extend(pubkey);
    o.extend(nonce.to_le_bytes());
    put_str(&mut o, receiver);
    o.extend(block_hash);
    o.extend(1u32.to_le_bytes()); // one action
    o.push(2); // Action::FunctionCall
    put_str(&mut o, METHOD);
    o.extend((args.len() as u32).to_le_bytes());
    o.extend(args);
    o.extend(GAS.to_le_bytes());
    o.extend(0u128.to_le_bytes()); // deposit: none, a function-call key cannot attach one
    o
}

/// Returns (signed transaction bytes, tx hash in base58).
pub fn sign_tx(key: &SigningKey, signer: &str, nonce: u64, receiver: &str, block_hash: &[u8; 32], args: &[u8]) -> (Vec<u8>, String) {
    let pk = key.verifying_key().to_bytes();
    let tx = tx_bytes(signer, &pk, nonce, receiver, block_hash, args);
    let hash = Sha256::digest(&tx);
    let sig = key.sign(&hash).to_bytes();
    let mut signed = tx;
    signed.push(0); // ed25519 signature
    signed.extend(sig);
    (signed, bs58::encode(hash).into_string())
}

/// `ed25519:<base58 of 64 bytes (seed + public)>` as printed by near-cli, or of the 32-byte seed.
pub fn parse_secret(s: &str) -> Result<SigningKey, String> {
    let b58 = s.strip_prefix("ed25519:").ok_or("key must start with ed25519:")?;
    let bytes = bs58::decode(b58).into_vec().map_err(|e| format!("bad base58: {e}"))?;
    let seed: [u8; 32] = match bytes.len() {
        64 | 32 => bytes[..32].try_into().unwrap(),
        n => return Err(format!("unexpected key length {n}")),
    };
    Ok(SigningKey::from_bytes(&seed))
}

pub fn parse_block_hash(s: &str) -> Result<[u8; 32], String> {
    let v = bs58::decode(s).into_vec().map_err(|e| format!("bad block hash: {e}"))?;
    v.try_into().map_err(|_| "block hash is not 32 bytes".to_string())
}

/// Whether a tick is worth sending: something is due at or before `now_ms`.
pub fn due(next_due_ms: Option<u64>, now_ms: u64) -> bool {
    matches!(next_due_ms, Some(t) if t <= now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_primitives::borsh;
    use near_primitives::transaction::{Action, FunctionCallAction, SignedTransaction, Transaction, TransactionV0};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn bytes_match_near_primitives_exactly() {
        let k = key();
        let pk = k.verifying_key().to_bytes();
        let bh = [9u8; 32];
        let args = br#"{"max_tasks":5}"#;
        let ours = tx_bytes("nme-operator.testnet", &pk, 42, "arena.nme.testnet", &bh, args);
        let theirs = Transaction::V0(TransactionV0 {
            signer_id: "nme-operator.testnet".parse().unwrap(),
            public_key: near_crypto::PublicKey::ED25519(near_crypto::ED25519PublicKey(pk)),
            nonce: 42,
            receiver_id: "arena.nme.testnet".parse().unwrap(),
            block_hash: near_primitives::hash::CryptoHash(bh),
            actions: vec![Action::FunctionCall(Box::new(FunctionCallAction { method_name: "tick".into(), args: args.to_vec(), gas: near_primitives::types::Gas::from_gas(GAS), deposit: near_primitives::types::Balance::from_yoctonear(0) }))],
        });
        assert_eq!(ours, borsh::to_vec(&theirs).unwrap());
    }

    #[test]
    fn signed_transaction_decodes_and_verifies() {
        let k = key();
        let (bytes, hash) = sign_tx(&k, "nme-operator.testnet", 5, "arena.nme.testnet", &[1u8; 32], br#"{"max_tasks":5}"#);
        let st: SignedTransaction = borsh::from_slice(&bytes).expect("a real SignedTransaction");
        assert_eq!(st.get_hash().to_string(), hash);
        assert!(st.signature.verify(st.get_hash().as_ref(), st.transaction.public_key()));
    }

    #[test]
    fn keys_and_hashes_parse() {
        let k = key();
        let mut full = k.to_bytes().to_vec();
        full.extend(k.verifying_key().to_bytes());
        let s = format!("ed25519:{}", bs58::encode(full).into_string());
        assert_eq!(parse_secret(&s).unwrap().to_bytes(), k.to_bytes());
        assert!(parse_secret("secp256k1:abc").is_err());
        assert!(parse_secret("ed25519:abc").is_err());
        assert!(parse_block_hash(&bs58::encode([3u8; 32]).into_string()).is_ok());
        assert!(parse_block_hash("abc").is_err());
    }

    #[test]
    fn only_an_overdue_task_triggers_a_tick() {
        assert!(!due(None, 100));
        assert!(!due(Some(101), 100));
        assert!(due(Some(100), 100));
        assert!(due(Some(1), 100));
    }
}
