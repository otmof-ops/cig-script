// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What makes a pass a pass: the tag chain of a local pass (HMAC-SHA256,
//! the macaroons construction) and the signature chain of a shareable one
//! (Ed25519 blocks, the Biscuit design). Issuing, narrowing and checking
//! the chain live here and nowhere else.

use crate::encode::{Block, Caveat, Issuer, Pass, Proof, Tag};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// The first link of a local pass's chain.
pub(crate) fn local_tag0(key: &[u8; 32], base: &[u8]) -> [u8; 32] {
    hmac(key, &[b"cigpass1-tag0", base])
}

/// The next link: computed from the last one, so anyone holding a pass can
/// add a caveat and nobody can take one away.
pub(crate) fn local_next(prev: &[u8; 32], caveat: &Caveat) -> [u8; 32] {
    hmac(prev, &[b"cigpass1-cav", &caveat.encoded()])
}

/// The whole chain of a local pass under `key`.
pub(crate) fn local_chain(key: &[u8; 32], pass: &Pass) -> [u8; 32] {
    let mut t = local_tag0(key, &pass.encode_base());
    for c in &pass.caveats {
        t = local_next(&t, c);
    }
    t
}

/// Does a local pass's tag hold under `key`? Constant time.
pub(crate) fn local_ok(key: &[u8; 32], pass: &Pass) -> bool {
    match &pass.tag {
        Tag::Local(t) => bool::from(local_chain(key, pass).ct_eq(t)),
        Tag::Shareable { .. } => false,
    }
}

/// A fresh Ed25519 key pair for one block.
pub(crate) fn fresh_signing_key() -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("no randomness for a key: {e}"))?;
    let k = SigningKey::from_bytes(&seed);
    seed.fill(0);
    Ok(k)
}

fn block_message(i: u32, prev: &[u8], body: &[u8], next_pk: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(16 + prev.len() + body.len() + 32);
    m.extend_from_slice(b"cigpass1-blk");
    m.extend_from_slice(&i.to_le_bytes());
    m.extend_from_slice(prev);
    m.extend_from_slice(body);
    m.extend_from_slice(next_pk);
    m
}

fn seal_message(last_sig: &[u8; 64]) -> Vec<u8> {
    let mut m = b"cigpass1-seal".to_vec();
    m.extend_from_slice(last_sig);
    m
}

/// Sign the first block of a shareable pass with the issuer's key: its
/// base and the first fresh public key.
pub(crate) fn share_first(issuer: &SigningKey, base: &[u8]) -> Result<(Block, SigningKey), String> {
    let next = fresh_signing_key()?;
    let next_pk = next.verifying_key().to_bytes();
    let sig = issuer.sign(&block_message(0, &[], base, &next_pk));
    Ok((
        Block {
            next_pk,
            sig: sig.to_bytes(),
        },
        next,
    ))
}

/// Add a caveat to a shareable pass with its proof key.
pub(crate) fn share_next(
    holder: &SigningKey,
    index: u32,
    prev: &Block,
    caveat: &Caveat,
) -> Result<(Block, SigningKey), String> {
    let next = fresh_signing_key()?;
    let next_pk = next.verifying_key().to_bytes();
    let sig = holder.sign(&block_message(
        index,
        &prev.sig,
        &caveat.encoded(),
        &next_pk,
    ));
    Ok((
        Block {
            next_pk,
            sig: sig.to_bytes(),
        },
        next,
    ))
}

/// Why a shareable pass's chain does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainFault {
    /// The issuer's key is not a valid Ed25519 key.
    IssuerKey,
    /// Block `n`'s signature does not verify.
    Block(usize),
    /// The proof does not match the last block.
    Proof,
    /// The pass is not a shareable one.
    NotShareable,
}

/// Check a shareable pass's whole chain: every block by the key before it,
/// and the proof against the last.
pub(crate) fn share_ok(pass: &Pass) -> Result<(), ChainFault> {
    let (blocks, proof) = match &pass.tag {
        Tag::Shareable { blocks, proof } => (blocks, proof),
        Tag::Local(_) => return Err(ChainFault::NotShareable),
    };
    let issuer = match pass.issuer {
        Issuer::Ed25519(pk) => pk,
        Issuer::Local(_) => return Err(ChainFault::NotShareable),
    };
    if blocks.len() != pass.caveats.len() + 1 {
        return Err(ChainFault::Block(blocks.len()));
    }
    let base = pass.encode_base();
    let mut key = VerifyingKey::from_bytes(&issuer).map_err(|_| ChainFault::IssuerKey)?;
    for (i, b) in blocks.iter().enumerate() {
        let msg = if i == 0 {
            block_message(0, &[], &base, &b.next_pk)
        } else {
            block_message(
                i as u32,
                &blocks[i - 1].sig,
                &pass.caveats[i - 1].encoded(),
                &b.next_pk,
            )
        };
        let sig = Signature::from_bytes(&b.sig);
        key.verify_strict(&msg, &sig)
            .map_err(|_| ChainFault::Block(i))?;
        key = VerifyingKey::from_bytes(&b.next_pk).map_err(|_| ChainFault::Block(i))?;
    }
    let last = blocks.last().expect("at least the first block");
    match proof {
        Proof::Key(sk) => {
            let pk = SigningKey::from_bytes(sk).verifying_key().to_bytes();
            if bool::from(pk.ct_eq(&last.next_pk)) {
                Ok(())
            } else {
                Err(ChainFault::Proof)
            }
        }
        Proof::Sealed(sig) => key
            .verify_strict(&seal_message(&last.sig), &Signature::from_bytes(sig))
            .map_err(|_| ChainFault::Proof),
    }
}

/// Close a shareable pass: nobody holding it can add a caveat after this.
pub fn seal(pass: &Pass) -> Result<Pass, String> {
    let mut p = pass.clone();
    match &mut p.tag {
        Tag::Shareable { blocks, proof } => {
            let sk = match proof {
                Proof::Key(k) => SigningKey::from_bytes(k),
                Proof::Sealed(_) => return Ok(pass.clone()),
            };
            let last = blocks.last().expect("at least the first block");
            let sig = sk.sign(&seal_message(&last.sig));
            *proof = Proof::Sealed(sig.to_bytes());
            Ok(p)
        }
        Tag::Local(_) => Err("a local pass has no signature chain to seal".to_string()),
    }
}

/// Add a caveat to a pass without any key but the one it carries: the
/// last tag of a local pass, or the proof key of a shareable one. A
/// sealed shareable pass cannot be narrowed.
pub fn attenuate(pass: &Pass, caveat: Caveat) -> Result<Pass, String> {
    if pass.caveats.len() >= crate::encode::MAX_CAVEATS {
        return Err(format!(
            "a pass holds at most {} caveats",
            crate::encode::MAX_CAVEATS
        ));
    }
    crate::issue::check_caveat(&caveat)?;
    let mut p = pass.clone();
    match &pass.tag {
        Tag::Local(t) => {
            let next = local_next(t, &caveat);
            p.caveats.push(caveat);
            p.tag = Tag::Local(next);
        }
        Tag::Shareable { blocks, proof } => {
            let holder = match proof {
                Proof::Key(k) => SigningKey::from_bytes(k),
                Proof::Sealed(_) => {
                    return Err(
                        "this pass is sealed: nobody holding it can narrow it further".to_string(),
                    )
                }
            };
            let last = blocks.last().expect("at least the first block");
            let (block, next) = share_next(&holder, blocks.len() as u32, last, &caveat)?;
            let mut blocks = blocks.clone();
            blocks.push(block);
            p.caveats.push(caveat);
            p.tag = Tag::Shareable {
                blocks,
                proof: Proof::Key(next.to_bytes()),
            };
        }
    }
    debug_assert_eq!(p.form, pass.form);
    Ok(p)
}

/// An Ed25519 key from a 32-byte seed (the machine's derived signing key).
pub fn signing_key(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(seed)
}

/// The public half of a signing seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}
