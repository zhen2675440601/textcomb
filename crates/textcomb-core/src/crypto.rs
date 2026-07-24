use crate::error::{CoreError, CoreResult, ErrorCode};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 24;

pub fn decode_master_key(encoded: &str) -> CoreResult<[u8; 32]> {
    let bytes = STANDARD.decode(encoded).map_err(|_| {
        CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "TEXTCOMB_MASTER_KEY_BASE64 必须是 Base64",
        )
    })?;
    bytes.try_into().map_err(|_| {
        CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "TEXTCOMB_MASTER_KEY_BASE64 解码后必须正好是 32 字节",
        )
    })
}

pub fn encrypt_secret(key: &[u8; 32], plaintext: &SecretString) -> CoreResult<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0_u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            plaintext.expose_secret().as_bytes(),
        )
        .map_err(|_| CoreError::public(ErrorCode::Internal, "敏感配置加密失败"))?;
    let mut output = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub fn decrypt_secret(key: &[u8; 32], encrypted: &[u8]) -> CoreResult<SecretString> {
    if encrypted.len() <= NONCE_LEN {
        return Err(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型密钥密文无效",
        ));
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&encrypted[..NONCE_LEN]),
            &encrypted[NONCE_LEN..],
        )
        .map_err(|_| {
            CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "无法使用当前主密钥解密模型密钥",
            )
        })?;
    String::from_utf8(plaintext)
        .map(SecretString::from)
        .map_err(|_| CoreError::public(ErrorCode::ConfigurationInvalid, "模型密钥不是 UTF-8"))
}

pub fn hash_password(password: &SecretString) -> CoreResult<String> {
    validate_password(password.expose_secret())?;
    let mut salt_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?;
    Argon2::default()
        .hash_password(password.expose_secret().as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))
}

pub fn verify_password(password: &SecretString, encoded_hash: &str) -> bool {
    PasswordHash::new(encoded_hash)
        .ok()
        .and_then(|hash| {
            Argon2::default()
                .verify_password(password.expose_secret().as_bytes(), &hash)
                .ok()
        })
        .is_some()
}

pub fn validate_password(password: &str) -> CoreResult<()> {
    let count = password.chars().count();
    if !(12..=128).contains(&count) {
        return Err(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "密码长度必须在 12 到 128 个字符之间",
        ));
    }
    Ok(())
}

pub fn new_session_token() -> (String, Vec<u8>) {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let raw = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash_token(&raw);
    (raw, hash)
}

pub fn hash_token(raw: &str) -> Vec<u8> {
    Sha256::digest(raw.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trip() {
        let key = [7_u8; 32];
        let value = SecretString::from("example-secret".to_owned());
        let encrypted = encrypt_secret(&key, &value).unwrap();
        assert_ne!(encrypted, value.expose_secret().as_bytes());
        assert_eq!(
            decrypt_secret(&key, &encrypted).unwrap().expose_secret(),
            value.expose_secret()
        );
    }

    #[test]
    fn password_round_trip() {
        let password = SecretString::from("a-long-test-password".to_owned());
        let hash = hash_password(&password).unwrap();
        assert!(verify_password(&password, &hash));
        assert!(!verify_password(
            &SecretString::from("another-password".to_owned()),
            &hash
        ));
    }
}
