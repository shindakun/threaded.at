// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! P-256 conversions between JWK and RustCrypto keys. These replace
//! jose-jwk's `crypto` feature, which pins p256 0.13 and with it
//! crypto-bigint 0.5 (RUSTSEC-2026-0387).

use jose_jwk::{Ec, EcCurves, Key};
use p256::elliptic_curve::sec1::ToSec1Point;
use rand::RngCore;

fn public_ec(pk: &p256::PublicKey, d: Option<Vec<u8>>) -> Key {
    let point = pk.to_sec1_point(false);
    Key::Ec(Ec {
        crv: EcCurves::P256,
        x: point.x().expect("uncompressed point has x").to_vec().into(),
        y: point.y().expect("uncompressed point has y").to_vec().into(),
        d: d.map(Into::into),
    })
}

/// The JWK for a secret key, with `x`, `y`, and `d`.
pub(crate) fn secret_to_key(sk: &p256::SecretKey) -> Key {
    public_ec(&sk.public_key(), Some(sk.to_bytes().to_vec()))
}

/// The JWK for a public key, with `x` and `y` only.
pub(crate) fn public_to_key(pk: &p256::PublicKey) -> Key {
    public_ec(pk, None)
}

/// The secret key in a P-256 JWK, if its `x` and `y` match `d`.
pub(crate) fn key_to_secret(key: &Key) -> Option<p256::SecretKey> {
    let Key::Ec(ec) = key else { return None };
    if ec.crv != EcCurves::P256 {
        return None;
    }
    let d: &[u8] = ec.d.as_ref()?.as_ref();
    let sk = p256::SecretKey::from_slice(d).ok()?;
    let point = sk.public_key().to_sec1_point(false);
    let (x, y): (&[u8], &[u8]) = (ec.x.as_ref(), ec.y.as_ref());
    (point.x()?.as_slice() == x && point.y()?.as_slice() == y).then_some(sk)
}

/// A fresh P-256 secret key from the operating system's CSPRNG.
pub(crate) fn generate() -> p256::SecretKey {
    let mut rng = rand::rngs::OsRng;
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        // Out-of-range scalars occur with probability below 2^-32.
        if let Ok(sk) = p256::SecretKey::from_slice(&bytes) {
            return sk;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_mismatch() {
        let sk = generate();
        let key = secret_to_key(&sk);
        assert_eq!(key_to_secret(&key).unwrap().to_bytes(), sk.to_bytes());

        let other = secret_to_key(&generate());
        let (Key::Ec(mut a), Key::Ec(b)) = (key, other) else { panic!() };
        a.x = b.x;
        assert!(key_to_secret(&Key::Ec(a)).is_none(), "x from another key must be rejected");
    }
}
