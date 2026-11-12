//! Signing one NEAR function call with a function-call-only key, used to upload a meme image to FastFS.
//! The key is restricted on-chain to `outlayer.testnet` / `__fastdata_fastfs` (gas only, no deposit, no other receiver),
//! so even a bug here cannot move value.
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

pub const FASTFS_RECEIVER: &str = "outlayer.testnet";
pub const FASTFS_METHOD: &str = "__fastdata_fastfs";
/// FastFS indexes the call data, not the execution: the call is meant to run out of gas.
pub const FASTFS_GAS: u64 = 1_000_000_000;
/// One transaction carries at most this much image (the chunked upload is not implemented).
pub const MAX_IMAGE_BYTES: usize = 900_000;

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// Borsh of the FastFS call data: `[0u8] ++ str(relative_path) ++ Some(str(mime) ++ Vec<u8>)` (docs/spikes/S4.md).
pub fn fastfs_payload(relative_path: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let mut o = vec![0u8];
    put_str(&mut o, relative_path);
    o.push(1);
    put_str(&mut o, mime);
    o.extend((data.len() as u32).to_le_bytes());
    o.extend(data);
    o
}

/// Borsh of a V0 transaction with one FunctionCall action and zero deposit.
pub fn tx_bytes(signer: &str, pubkey: &[u8; 32], nonce: u64, receiver: &str, block_hash: &[u8; 32], method: &str, args: &[u8], gas: u64) -> Vec<u8> {
    let mut o = Vec::new();
    put_str(&mut o, signer);
    o.push(0);
    o.extend(pubkey);
    o.extend(nonce.to_le_bytes());
    put_str(&mut o, receiver);
    o.extend(block_hash);
    o.extend(1u32.to_le_bytes());
    o.push(2);
    put_str(&mut o, method);
    o.extend((args.len() as u32).to_le_bytes());
    o.extend(args);
    o.extend(gas.to_le_bytes());
    o.extend(0u128.to_le_bytes());
    o
}

pub fn sign_call(key: &SigningKey, signer: &str, nonce: u64, block_hash: &[u8; 32], args: &[u8]) -> Vec<u8> {
    let pk = key.verifying_key().to_bytes();
    let tx = tx_bytes(signer, &pk, nonce, FASTFS_RECEIVER, block_hash, FASTFS_METHOD, args, FASTFS_GAS);
    let sig = key.sign(&Sha256::digest(&tx)).to_bytes();
    let mut signed = tx;
    signed.push(0);
    signed.extend(sig);
    signed
}

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
    bs58::decode(s).into_vec().map_err(|e| format!("bad block hash: {e}"))?.try_into().map_err(|_| "block hash is not 32 bytes".to_string())
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Public URL FastFS serves the upload at.
pub fn fastfs_url(account: &str, relative_path: &str) -> String {
    format!("https://test.fastfs.io/{account}/{FASTFS_RECEIVER}/{relative_path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_primitives::borsh;
    use near_primitives::transaction::{Action, FunctionCallAction, SignedTransaction, Transaction, TransactionV0};

    #[test]
    fn the_transaction_is_exactly_what_near_primitives_encodes_and_verifies() {
        let k = SigningKey::from_bytes(&[5u8; 32]);
        let pk = k.verifying_key().to_bytes();
        let args = fastfs_payload("abc.jpg", "image/jpeg", &[1, 2, 3, 4]);
        let ours = tx_bytes("nme-media.testnet", &pk, 7, FASTFS_RECEIVER, &[9u8; 32], FASTFS_METHOD, &args, FASTFS_GAS);
        let theirs = Transaction::V0(TransactionV0 {
            signer_id: "nme-media.testnet".parse().unwrap(),
            public_key: near_crypto::PublicKey::ED25519(near_crypto::ED25519PublicKey(pk)),
            nonce: 7,
            receiver_id: FASTFS_RECEIVER.parse().unwrap(),
            block_hash: near_primitives::hash::CryptoHash([9u8; 32]),
            actions: vec![Action::FunctionCall(Box::new(FunctionCallAction { method_name: FASTFS_METHOD.into(), args: args.clone(), gas: near_primitives::types::Gas::from_gas(FASTFS_GAS), deposit: near_primitives::types::Balance::from_yoctonear(0) }))],
        });
        assert_eq!(ours, borsh::to_vec(&theirs).unwrap());
        let signed = sign_call(&k, "nme-media.testnet", 7, &[9u8; 32], &args);
        let st: SignedTransaction = borsh::from_slice(&signed).unwrap();
        assert!(st.signature.verify(st.get_hash().as_ref(), st.transaction.public_key()));
    }

    #[test]
    fn the_fastfs_payload_has_the_documented_layout() {
        let p = fastfs_payload("a.jpg", "image/jpeg", &[0xff, 0xd8]);
        assert_eq!(p[0], 0);
        assert_eq!(&p[1..5], &5u32.to_le_bytes());
        assert_eq!(&p[5..10], b"a.jpg");
        assert_eq!(p[10], 1, "Some(file)");
        assert_eq!(&p[11..15], &10u32.to_le_bytes());
        assert_eq!(&p[15..25], b"image/jpeg");
        assert_eq!(&p[25..29], &2u32.to_le_bytes());
        assert_eq!(&p[29..], &[0xff, 0xd8]);
    }

    #[test]
    fn helpers() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(fastfs_url("nme-media.testnet", "x.jpg"), "https://test.fastfs.io/nme-media.testnet/outlayer.testnet/x.jpg");
        assert!(parse_secret("rsa:abc").is_err() && parse_secret("ed25519:zz").is_err());
        assert!(parse_block_hash("abc").is_err());
    }
}
