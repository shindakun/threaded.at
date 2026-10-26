//! The ES256 signatures jacquard-oauth makes (DPoP proofs and client
//! assertions) verify with an independent P-256 verifier.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jacquard_oauth::jose::jwt::Claims;
use jacquard_oauth::keyset::Keyset;
use jose_jwa::Signing;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};

fn b64(s: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(s).expect("base64url")
}

fn verifying_key(x: &str, y: &str) -> VerifyingKey {
    let mut sec1 = vec![0x04];
    sec1.extend(b64(x));
    sec1.extend(b64(y));
    VerifyingKey::from_sec1_bytes(&sec1).expect("valid public key")
}

fn verify(jwt: &str, key: &VerifyingKey) -> serde_json::Value {
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let sig = Signature::from_slice(&b64(parts[2])).expect("64-byte signature");
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .expect("signature verifies");
    let tampered = format!("{}.{}x", parts[0], parts[1]);
    assert!(
        key.verify(tampered.as_bytes(), &sig).is_err(),
        "tampered input must not verify"
    );
    serde_json::from_slice(&b64(parts[0])).unwrap()
}

#[test]
fn dpop_proof_signature_verifies_with_embedded_key() {
    let key = jacquard_oauth::utils::generate_key(&["ES256"]).expect("ES256 key");
    let proof = jacquard_oauth::dpop::build_dpop_proof(
        &key,
        "POST",
        "https://bsky.social/oauth/par",
        Some("n"),
        None,
    )
    .unwrap();
    let header: serde_json::Value =
        serde_json::from_slice(&b64(proof.split('.').next().unwrap())).unwrap();
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["typ"], "dpop+jwt");
    let jwk = &header["jwk"];
    assert!(
        jwk.get("d").is_none(),
        "the proof must not carry the private key"
    );
    let vk = verifying_key(jwk["x"].as_str().unwrap(), jwk["y"].as_str().unwrap());
    verify(&proof, &vk);
}

#[test]
fn client_assertion_signature_verifies_with_published_key() {
    let keyset = Keyset::generate_es256("threaded-1").unwrap();
    let jwt = keyset
        .create_jwt(&[Signing::Es256], Claims::default())
        .unwrap();
    let jwks = serde_json::to_value(keyset.public_jwks()).unwrap();
    let pubkey = &jwks["keys"][0];
    assert!(
        pubkey.get("d").is_none(),
        "the published key must not carry the private key"
    );
    let vk = verifying_key(pubkey["x"].as_str().unwrap(), pubkey["y"].as_str().unwrap());
    let header = verify(&jwt, &vk);
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["kid"], "threaded-1");
}

#[test]
fn saved_keys_reload_and_still_sign() {
    let dir = tempfile::tempdir().unwrap();
    threaded_auth::keys::generate(dir.path()).unwrap();
    let keyset =
        threaded_auth::keys::load_keyset(&dir.path().join(threaded_auth::keys::OAUTH_KEY_FILE))
            .unwrap();
    let jwt = keyset
        .create_jwt(&[Signing::Es256], Claims::default())
        .unwrap();
    let pubkey = serde_json::to_value(keyset.public_jwks()).unwrap()["keys"][0].clone();
    verify(
        &jwt,
        &verifying_key(pubkey["x"].as_str().unwrap(), pubkey["y"].as_str().unwrap()),
    );
}
