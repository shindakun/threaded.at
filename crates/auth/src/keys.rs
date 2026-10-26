use std::io::Write;
use std::path::Path;

use jacquard_oauth::keyset::Keyset;
use rand::RngCore;

pub const OAUTH_KEY_FILE: &str = "oauth-key.json";
pub const COOKIE_KEY_FILE: &str = "cookie.key";
pub const COOKIE_KEY_LEN: usize = 64;
const KEY_ID: &str = "threaded-1";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0} already exists; keygen never overwrites keys")]
    Exists(String),
    #[error("{0}: not a valid key file: {1}")]
    Invalid(String, String),
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), KeyError> {
    let shown = path.display().to_string();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => KeyError::Exists(shown.clone()),
        _ => KeyError::Io(shown.clone(), e),
    })?;
    f.write_all(bytes).map_err(|e| KeyError::Io(shown, e))
}

/// Writes `oauth-key.json` and `cookie.key` into `dir`. Fails without writing
/// anything when either file exists.
pub fn generate(dir: &Path) -> Result<(), KeyError> {
    for name in [OAUTH_KEY_FILE, COOKIE_KEY_FILE] {
        let p = dir.join(name);
        if p.exists() {
            return Err(KeyError::Exists(p.display().to_string()));
        }
    }
    let keyset = Keyset::generate_es256(KEY_ID)
        .map_err(|e| KeyError::Invalid(OAUTH_KEY_FILE.into(), e.to_string()))?;
    let jwks: Vec<_> = keyset.into();
    let json = serde_json::to_vec_pretty(&jwks).expect("JWK set serializes");
    let mut cookie = vec![0u8; COOKIE_KEY_LEN];
    rand::rng().fill_bytes(&mut cookie);

    write_new(&dir.join(OAUTH_KEY_FILE), &json)?;
    write_new(&dir.join(COOKIE_KEY_FILE), &cookie)
}

pub fn load_keyset(path: &Path) -> Result<Keyset, KeyError> {
    let shown = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| KeyError::Io(shown.clone(), e))?;
    let jwks: Vec<_> = serde_json::from_slice(&bytes)
        .map_err(|e| KeyError::Invalid(shown.clone(), e.to_string()))?;
    Keyset::try_from(jwks).map_err(|e| KeyError::Invalid(shown, e.to_string()))
}

pub fn load_cookie_key(path: &Path) -> Result<Vec<u8>, KeyError> {
    let shown = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| KeyError::Io(shown.clone(), e))?;
    if bytes.len() != COOKIE_KEY_LEN {
        return Err(KeyError::Invalid(
            shown,
            format!("expected {COOKIE_KEY_LEN} bytes, found {}", bytes.len()),
        ));
    }
    Ok(bytes)
}

pub fn random_cookie_key() -> Vec<u8> {
    let mut k = vec![0u8; COOKIE_KEY_LEN];
    rand::rng().fill_bytes(&mut k);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_load() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path()).unwrap();
        let ks = load_keyset(&dir.path().join(OAUTH_KEY_FILE)).unwrap();
        assert_eq!(ks.public_jwks().keys.len(), 1);
        assert_eq!(
            load_cookie_key(&dir.path().join(COOKIE_KEY_FILE))
                .unwrap()
                .len(),
            COOKIE_KEY_LEN
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [OAUTH_KEY_FILE, COOKIE_KEY_FILE] {
                let mode = std::fs::metadata(dir.path().join(f))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{f}");
            }
        }
    }

    #[test]
    fn never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path()).unwrap();
        let before = std::fs::read(dir.path().join(OAUTH_KEY_FILE)).unwrap();
        assert!(matches!(generate(dir.path()), Err(KeyError::Exists(_))));
        assert_eq!(
            std::fs::read(dir.path().join(OAUTH_KEY_FILE)).unwrap(),
            before
        );
    }

    #[test]
    fn short_cookie_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k");
        std::fs::write(&p, [1u8; 10]).unwrap();
        assert!(matches!(load_cookie_key(&p), Err(KeyError::Invalid(..))));
    }
}
