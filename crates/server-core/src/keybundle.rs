//! E2EE prekey-bundle service (Wave 13): publish, fetch, and atomic consume.
//!
//! A thin domain layer over [`rabbithole_store_server::repo8::KeyBundlesRepo`].
//! It validates the shape of the **public** key material a client publishes
//! (fixed X25519 / Ed25519 sizes) and exposes a fetch that atomically hands out
//! one one-time prekey per call. The server never sees a private key or any
//! plaintext; this service only ever moves public bytes.

use rabbithole_store_server::repo8::{BundleRow, KeyBundlesRepo};
use rabbithole_store_server::{SqlitePool, StoreError};

/// Byte length of an X25519 / Ed25519 public key.
const KEY_LEN: usize = 32;
/// Byte length of an Ed25519 signature.
const SIG_LEN: usize = 64;
/// Cap on how many one-time prekeys a single publish may deposit.
const MAX_ONE_TIME_PREKEYS: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum KeyBundleError {
    #[error("malformed prekey bundle (bad key/signature length or too many prekeys)")]
    BadInput,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Publish/fetch service for E2EE prekey bundles.
pub struct KeyBundleService;

impl KeyBundleService {
    /// Validate and store `account`'s bundle, replacing any prior one and its
    /// one-time-prekey pool.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish(
        pool: &SqlitePool,
        account: i64,
        identity_key: &[u8],
        signing_key: &[u8],
        signed_prekey: &[u8],
        signed_prekey_sig: &[u8],
        one_time_prekeys: &[[u8; 32]],
        now: i64,
    ) -> Result<(), KeyBundleError> {
        if identity_key.len() != KEY_LEN
            || signing_key.len() != KEY_LEN
            || signed_prekey.len() != KEY_LEN
            || signed_prekey_sig.len() != SIG_LEN
            || one_time_prekeys.len() > MAX_ONE_TIME_PREKEYS
        {
            return Err(KeyBundleError::BadInput);
        }
        KeyBundlesRepo(pool)
            .publish(
                account,
                identity_key,
                signing_key,
                signed_prekey,
                signed_prekey_sig,
                one_time_prekeys,
                now,
            )
            .await?;
        Ok(())
    }

    /// Fetch `account`'s bundle, atomically consuming one one-time prekey.
    /// `None` when the account has published no bundle.
    pub async fn fetch_consume(
        pool: &SqlitePool,
        account: i64,
    ) -> Result<Option<BundleRow>, KeyBundleError> {
        Ok(KeyBundlesRepo(pool).fetch_consume(account).await?)
    }
}
