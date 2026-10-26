//! Checking OpenPGP certificates from the server or a contact card, and
//! choosing which of their keys to use.
//!
//! Zmail's own code, not a port. go-crypto (`ProtonMail/go-crypto`
//! v1.4.1-proton, which gopenpgp v2.10.0-proton and so Bridge v3.27.1 use)
//! checks a certificate's self-signatures as it reads it (`openpgp/keys.go`:
//! `ReadEntity`, `addUserID`, `addSubkey`, and `VerifyKeySignature` in
//! `packet/public_key.go`), and gopenpgp picks keys with
//! `Entity.EncryptionKey(now)` and `Entity.SigningKey(now)`. rPGP parses
//! without checking anything, so this module does both on rPGP's
//! primitives (ADR 0004, "Verifying keys").
//!
//! **Verify on parse** ([`verify_public`], [`verify_secret`]). Every
//! signature that does not verify is dropped, and what is left is the
//! certificate the rest of Zmail sees:
//!
//! - a user ID keeps only the self-certifications (and certification
//!   revocations) the primary key made over it; a user ID with no valid
//!   self-certification is dropped, as are user attributes, which go-crypto
//!   ignores;
//! - key revocations and direct-key signatures must verify against the
//!   primary key;
//! - a subkey keeps only the binding and revocation signatures the primary
//!   key made over it, and a binding that gives the subkey the sign flag
//!   also needs a valid embedded primary-key binding (back-signature) by
//!   the subkey; a subkey without a valid binding is dropped;
//! - the public-only subkeys of a secret key are dropped: none of them can
//!   decrypt, and Zmail never encrypts to itself with a key it cannot
//!   decrypt with.
//!
//! The certificate is refused if its primary key cannot sign, or if no
//! self-signature for the primary key is left: a valid self-certification
//! of a user ID for a v4 key, a valid direct-key signature for a v6 key.
//!
//! go-crypto refuses the whole certificate when any self-signature fails;
//! dropping just the failed parts is no weaker, since a dropped signature
//! can neither add a key nor remove a restriction, and it keeps a key
//! usable when someone appends garbage to it.
//!
//! **Selection** ([`encryption_key`], [`signing_key`]) mirrors
//! `EncryptionKey` and `signingKeyByIdUsage` with `now` the current time:
//! nothing is usable when the primary key's self-signature (the newest
//! self-certification of the primary user ID, or the newest direct-key
//! signature of a v6 key) is missing or expired, when the primary key has
//! expired or is revoked, or when the primary user ID is revoked. Of the
//! subkeys whose newest binding carries key flags with the needed usage
//! (encrypt communications, or sign), whose algorithm can do it, that have
//! not expired and are not revoked, the one with the newest binding wins;
//! the primary key is used only when no subkey qualifies and its own
//! self-signature has that flag. Encrypt-storage alone does not qualify
//! and a missing key flags subpacket qualifies for nothing, as in
//! go-crypto. Its preference for post-quantum subkeys does not apply:
//! Zmail builds rPGP without them.

use pgp::{
    composed::{SignedKeyDetails, SignedPublicKey, SignedPublicSubKey, SignedSecretKey},
    packet::{
        KeyFlags, PublicKey, PublicSubkey, RevocationCode, Signature, SignatureType, SubpacketData,
    },
    types::{KeyDetails, KeyVersion, SignedUser, Tag, Timestamp},
};

use crate::response::Error;

/// What a key is wanted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Usage {
    /// Encrypting mail to the certificate's owner.
    Encrypt,
    /// Signing mail as the certificate's owner.
    Sign,
}

/// The key of a certificate that [`encryption_key`] or [`signing_key`]
/// chose: the primary key, or one of the subkeys.
#[derive(Debug)]
pub(crate) enum Selected<'a, S> {
    /// The primary key.
    Primary,
    /// A subkey.
    Subkey(&'a S),
}

/// Checks a public certificate's signatures (see the module docs): the
/// certificate with only its valid parts, or an error if its primary key
/// has no valid self-signature.
pub(crate) fn verify_public(key: SignedPublicKey) -> Result<SignedPublicKey, Error> {
    let SignedPublicKey {
        primary_key,
        details,
        public_subkeys,
    } = key;
    let details = verify_details(&primary_key, details)?;
    let public_subkeys = public_subkeys
        .into_iter()
        .filter_map(|subkey| {
            let signatures = verified_bindings(&primary_key, &subkey.key, subkey.signatures);
            (!signatures.is_empty()).then(|| SignedPublicSubKey {
                key: subkey.key,
                signatures,
            })
        })
        .collect();
    Ok(SignedPublicKey {
        primary_key,
        details,
        public_subkeys,
    })
}

/// Checks a secret certificate's signatures as [`verify_public`] does, and
/// drops its public-only subkeys.
pub(crate) fn verify_secret(key: SignedSecretKey) -> Result<SignedSecretKey, Error> {
    let SignedSecretKey {
        primary_key,
        details,
        public_subkeys: _,
        secret_subkeys,
    } = key;
    let primary = primary_key.public_key().clone();
    let details = verify_details(&primary, details)?;
    let secret_subkeys = secret_subkeys
        .into_iter()
        .filter_map(|mut subkey| {
            subkey.signatures =
                verified_bindings(&primary, subkey.key.public_key(), subkey.signatures);
            (!subkey.signatures.is_empty()).then_some(subkey)
        })
        .collect();
    Ok(SignedSecretKey {
        primary_key,
        details,
        public_subkeys: Vec::new(),
        secret_subkeys,
    })
}

/// The user IDs, revocations and direct-key signatures that verify, or an
/// error if the primary key is left without a self-signature (`ReadEntity`,
/// `addUserID`).
fn verify_details(
    primary: &PublicKey,
    details: SignedKeyDetails,
) -> Result<SignedKeyDetails, Error> {
    // `ReadEntity`: "primary key cannot be used for signatures".
    if !primary.algorithm().can_sign() {
        return Err(Error::Key(
            "the key's primary key cannot make signatures".to_owned(),
        ));
    }
    let users: Vec<SignedUser> = details
        .users
        .into_iter()
        .filter_map(|user| {
            let signatures: Vec<Signature> = user
                .signatures
                .into_iter()
                .filter(|signature| {
                    signature.created().is_some()
                        && signature
                            .verify_certification(primary, Tag::UserId, &user.id)
                            .is_ok()
                })
                .collect();
            signatures
                .iter()
                .any(|signature| signature.typ() != Some(SignatureType::CertRevocation))
                .then(|| SignedUser {
                    id: user.id,
                    signatures,
                })
        })
        .collect();
    let verifies = |signature: &Signature, typ: SignatureType| {
        signature.typ() == Some(typ)
            && signature.created().is_some()
            && signature.verify_key(primary).is_ok()
    };
    let revocation_signatures: Vec<Signature> = details
        .revocation_signatures
        .into_iter()
        .filter(|signature| verifies(signature, SignatureType::KeyRevocation))
        .collect();
    let direct_signatures: Vec<Signature> = details
        .direct_signatures
        .into_iter()
        .filter(|signature| verifies(signature, SignatureType::Key))
        .collect();
    let has_self_signature = if primary.version() == KeyVersion::V6 {
        !direct_signatures.is_empty()
    } else {
        !users.is_empty()
    };
    if !has_self_signature {
        return Err(Error::Key(
            "the key's self-signature does not verify".to_owned(),
        ));
    }
    Ok(SignedKeyDetails {
        revocation_signatures,
        direct_signatures,
        users,
        user_attributes: Vec::new(),
    })
}

/// A subkey's binding and revocation signatures that verify, with the
/// back-signature a signing subkey's binding needs (`addSubkey`,
/// `VerifyKeySignature`).
fn verified_bindings(
    primary: &PublicKey,
    subkey: &PublicSubkey,
    signatures: Vec<Signature>,
) -> Vec<Signature> {
    signatures
        .into_iter()
        .filter(|signature| {
            if signature.created().is_none()
                || signature.verify_subkey_binding(primary, subkey).is_err()
            {
                return false;
            }
            match signature.typ() {
                Some(SignatureType::SubkeyRevocation) => true,
                // "Signing subkeys must be cross-signed."
                Some(SignatureType::SubkeyBinding) => {
                    !key_flags(signature).is_some_and(|flags| flags.sign())
                        || signature.embedded_signature().is_some_and(|backsig| {
                            backsig.verify_primary_key_binding(subkey, primary).is_ok()
                        })
                }
                _ => false,
            }
        })
        .collect()
}

/// The key to encrypt to (`Entity.EncryptionKey`).
pub(crate) fn encryption_key<'a, S>(
    primary: &PublicKey,
    details: &SignedKeyDetails,
    subkeys: &'a [S],
    parts: impl Fn(&S) -> (&PublicSubkey, &[Signature]),
    now: Timestamp,
) -> Option<Selected<'a, S>> {
    select(primary, details, subkeys, parts, Usage::Encrypt, now)
}

/// The key to sign with (`Entity.SigningKey`).
pub(crate) fn signing_key<'a, S>(
    primary: &PublicKey,
    details: &SignedKeyDetails,
    subkeys: &'a [S],
    parts: impl Fn(&S) -> (&PublicSubkey, &[Signature]),
    now: Timestamp,
) -> Option<Selected<'a, S>> {
    select(primary, details, subkeys, parts, Usage::Sign, now)
}

/// The keys whose signatures can be checked: the primary key when its
/// self-signature has the sign flag, and the subkeys whose binding has it
/// (go-crypto's `KeysByIdUsage` with `KeyFlagSign`, which verification
/// goes through).
pub(crate) fn verifying_subkeys<S>(
    subkeys: &[S],
    parts: impl Fn(&S) -> (&PublicSubkey, &[Signature]),
) -> impl Iterator<Item = &S> {
    subkeys.iter().filter(move |subkey| {
        let (_, signatures) = parts(subkey);
        newest_binding(signatures).is_some_and(|binding| allows(binding, Usage::Sign))
    })
}

/// Whether the primary key may verify signatures (see [`verifying_subkeys`]).
pub(crate) fn primary_verifies(details: &SignedKeyDetails, primary: &PublicKey) -> bool {
    primary.algorithm().can_sign()
        && primary_self_signature(primary, details)
            .is_some_and(|(signature, _)| allows(signature, Usage::Sign))
}

fn select<'a, S>(
    primary: &PublicKey,
    details: &SignedKeyDetails,
    subkeys: &'a [S],
    parts: impl Fn(&S) -> (&PublicSubkey, &[Signature]),
    usage: Usage,
    now: Timestamp,
) -> Option<Selected<'a, S>> {
    // "Fail to find any encryption key if the..."
    let (self_signature, identity) = primary_self_signature(primary, details)?;
    if key_expired(primary.created_at(), self_signature, now)
        || revoked(details.revocation_signatures.iter(), now)
        || signature_expired(self_signature, now)
        || identity.is_some_and(|identity| revoked(identity.revocations(), now))
    {
        return None;
    }

    // "Iterate the keys to find the newest, unexpired one."
    let mut candidate: Option<(&S, Timestamp)> = None;
    for subkey in subkeys {
        let (key, signatures) = parts(subkey);
        let Some(binding) = newest_binding(signatures) else {
            continue;
        };
        let Some(created) = binding.created() else {
            continue;
        };
        let revocations = signatures
            .iter()
            .filter(|signature| signature.typ() == Some(SignatureType::SubkeyRevocation));
        if allows(binding, usage)
            && capable(key.algorithm(), usage)
            && !key_expired(key.created_at(), binding, now)
            && !signature_expired(binding, now)
            && !revoked(revocations, now)
            && candidate.is_none_or(|(_, newest)| created > newest)
        {
            candidate = Some((subkey, created));
        }
    }
    if let Some((subkey, _)) = candidate {
        return Some(Selected::Subkey(subkey));
    }

    // "If we don't have any subkeys for encryption and the primary key is
    // marked as OK to encrypt with, then we can use it."
    (allows(self_signature, usage) && capable(primary.algorithm(), usage))
        .then_some(Selected::Primary)
}

/// A user ID with its newest self-certification and its revocations
/// (go-crypto's `Identity`).
struct Identity<'a> {
    self_signature: &'a Signature,
    user: &'a SignedUser,
}

impl<'a> Identity<'a> {
    fn new(user: &'a SignedUser) -> Option<Self> {
        let self_signature = newest(
            user.signatures
                .iter()
                .filter(|signature| signature.typ() != Some(SignatureType::CertRevocation)),
        )?;
        Some(Self {
            self_signature,
            user,
        })
    }

    fn revocations(&self) -> impl Iterator<Item = &'a Signature> + use<'a> {
        self.user
            .signatures
            .iter()
            .filter(|signature| signature.typ() == Some(SignatureType::CertRevocation))
    }

    /// `shouldPreferIdentity`: fewer revocations, then the primary user ID
    /// flag, then the newer self-certification.
    fn preferred_over(&self, existing: &Identity<'_>) -> bool {
        let (ours, theirs) = (self.revocations().count(), existing.revocations().count());
        if theirs != ours {
            return ours < theirs;
        }
        match (
            existing.self_signature.is_primary(),
            self.self_signature.is_primary(),
        ) {
            (true, false) => false,
            (false, true) => true,
            _ => self.self_signature.created() > existing.self_signature.created(),
        }
    }
}

/// The self-signature that holds the primary key's properties, and for a
/// v4 key the user ID it certifies (`PrimarySelfSignature`).
fn primary_self_signature<'a>(
    primary: &PublicKey,
    details: &'a SignedKeyDetails,
) -> Option<(&'a Signature, Option<Identity<'a>>)> {
    if primary.version() == KeyVersion::V6 {
        return newest(details.direct_signatures.iter()).map(|signature| (signature, None));
    }
    let identity =
        details
            .users
            .iter()
            .filter_map(Identity::new)
            .reduce(|existing, identity| {
                if identity.preferred_over(&existing) {
                    identity
                } else {
                    existing
                }
            })?;
    Some((identity.self_signature, Some(identity)))
}

/// A subkey's newest binding signature (`addSubkey`).
fn newest_binding(signatures: &[Signature]) -> Option<&Signature> {
    newest(
        signatures
            .iter()
            .filter(|signature| signature.typ() == Some(SignatureType::SubkeyBinding)),
    )
}

/// The newest of `signatures` by creation time; the first wins a tie, as
/// go-crypto only replaces on `CreationTime.After`.
fn newest<'a>(signatures: impl Iterator<Item = &'a Signature>) -> Option<&'a Signature> {
    signatures.fold(None, |newest: Option<&Signature>, signature| match newest {
        Some(current) if signature.created() <= current.created() => Some(current),
        _ => Some(signature),
    })
}

/// The key flags subpacket of a signature, if it has one (go-crypto's
/// `FlagsValid`).
fn key_flags(signature: &Signature) -> Option<KeyFlags> {
    signature
        .config()?
        .hashed_subpackets()
        .find_map(|subpacket| match &subpacket.data {
            SubpacketData::KeyFlags(flags) => Some(flags.clone()),
            _ => None,
        })
}

/// Whether a self-signature's key flags allow `usage`.
fn allows(signature: &Signature, usage: Usage) -> bool {
    key_flags(signature).is_some_and(|flags| match usage {
        Usage::Encrypt => flags.encrypt_comms(),
        Usage::Sign => flags.sign(),
    })
}

fn capable(algorithm: pgp::crypto::public_key::PublicKeyAlgorithm, usage: Usage) -> bool {
    match usage {
        Usage::Encrypt => algorithm.can_encrypt(),
        Usage::Sign => algorithm.can_sign(),
    }
}

/// `PublicKey.KeyExpired`: created in the future, or past the key
/// expiration time the self-signature gives.
fn key_expired(created: Timestamp, signature: &Signature, now: Timestamp) -> bool {
    if created > now {
        return true;
    }
    match signature.key_expiration_time() {
        Some(lifetime) if lifetime.as_secs() != 0 => {
            u64::from(now.as_secs()) > u64::from(created.as_secs()) + u64::from(lifetime.as_secs())
        }
        _ => false,
    }
}

/// `Signature.SigExpired`: made in the future, or past its own expiration
/// time.
fn signature_expired(signature: &Signature, now: Timestamp) -> bool {
    let Some(created) = signature.created() else {
        return true;
    };
    if created > now {
        return true;
    }
    match signature.signature_expiration_time() {
        Some(lifetime) if lifetime.as_secs() != 0 => {
            u64::from(now.as_secs()) > u64::from(created.as_secs()) + u64::from(lifetime.as_secs())
        }
        _ => false,
    }
}

/// go-crypto's `revoked`: a revocation for a compromised key counts even
/// before its date; any other counts once it is in effect.
fn revoked<'a>(revocations: impl IntoIterator<Item = &'a Signature>, now: Timestamp) -> bool {
    revocations.into_iter().any(|revocation| {
        revocation.revocation_reason_code() == Some(&RevocationCode::KeyCompromised)
            || !signature_expired(revocation, now)
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Certificates for hostile tests, built with rPGP: a real key with
    //! subkeys whose binding signatures the tests replace.

    use pgp::{
        composed::{
            EncryptionCaps, KeyType, SecretKeyParamsBuilder, SignedSecretKey, SignedSecretSubKey,
            SubkeyParamsBuilder,
        },
        crypto::{ecc_curve::ECCCurve, hash::HashAlgorithm},
        packet::{
            KeyFlags, RevocationCode, SecretSubkey, Signature, SignatureConfig, SignatureType,
            Subpacket, SubpacketData,
        },
        types::{Duration, KeyDetails, Password, SigningKey, Tag, Timestamp},
    };

    /// A day, in seconds.
    pub(crate) const DAY: u32 = 86_400;

    /// The current time, in seconds.
    pub(crate) fn now() -> u32 {
        Timestamp::now().as_secs()
    }

    /// An unlocked v4 key in Proton's shape: an EdDSA primary key that
    /// certifies (and signs, if `primary_signs`), `encryption` ECDH
    /// Curve25519 subkeys and `signing` EdDSA subkeys, all made a week ago.
    pub(crate) fn generate(
        primary_signs: bool,
        encryption: usize,
        signing: usize,
    ) -> SignedSecretKey {
        let created = Timestamp::from_secs(now() - 7 * DAY);
        let mut builder = SecretKeyParamsBuilder::default();
        builder
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(primary_signs)
            .created_at(created)
            .primary_user_id("Fixture <fixture@example.org>".into());
        for _ in 0..encryption {
            builder.subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519Legacy))
                    .can_encrypt(EncryptionCaps::Communication)
                    .created_at(created)
                    .build()
                    .expect("subkey params"),
            );
        }
        for _ in 0..signing {
            builder.subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::Ed25519Legacy)
                    .can_sign(true)
                    .created_at(created)
                    .build()
                    .expect("subkey params"),
            );
        }
        builder
            .build()
            .expect("key params")
            .generate(&mut rand_pgp::rng())
            .expect("key")
    }

    /// What a binding signature says.
    #[derive(Clone, Copy)]
    pub(crate) struct Binding {
        /// When it was made, in seconds.
        pub(crate) created: u32,
        /// The key flags.
        pub(crate) encrypt: bool,
        pub(crate) sign: bool,
        /// The key expiration time, in seconds after the subkey's creation.
        pub(crate) expires_after: Option<u32>,
        /// For a signing subkey: embed a back-signature.
        pub(crate) backsig: bool,
    }

    impl Binding {
        /// An encryption subkey's binding made `days_ago`.
        pub(crate) fn encrypt(days_ago: u32) -> Self {
            Self {
                created: now() - days_ago * DAY,
                encrypt: true,
                sign: false,
                expires_after: None,
                backsig: false,
            }
        }

        /// A signing subkey's binding made `days_ago`, with its
        /// back-signature.
        pub(crate) fn sign(days_ago: u32) -> Self {
            Self {
                created: now() - days_ago * DAY,
                encrypt: false,
                sign: true,
                expires_after: None,
                backsig: true,
            }
        }
    }

    fn hashed(key: &SignedSecretKey, created: u32, mut more: Vec<SubpacketData>) -> Vec<Subpacket> {
        let mut data = vec![
            SubpacketData::SignatureCreationTime(Timestamp::from_secs(created)),
            SubpacketData::IssuerFingerprint(key.fingerprint()),
        ];
        data.append(&mut more);
        data.into_iter()
            .map(|data| Subpacket::regular(data).expect("subpacket"))
            .collect()
    }

    /// A binding of `subkey` to `key`'s primary key, signed by `signer`:
    /// the primary key itself for a valid binding, any other key for a
    /// forged one (which still names the primary key as its issuer).
    pub(crate) fn binding(
        key: &SignedSecretKey,
        signer: &impl SigningKey,
        subkey: &SecretSubkey,
        binding: Binding,
    ) -> Signature {
        let mut flags = KeyFlags::default();
        flags.set_encrypt_comms(binding.encrypt);
        flags.set_sign(binding.sign);
        let mut more = vec![SubpacketData::KeyFlags(flags)];
        if let Some(seconds) = binding.expires_after {
            more.push(SubpacketData::KeyExpirationTime(Duration::from_secs(
                seconds,
            )));
        }
        if binding.backsig {
            let mut config = SignatureConfig::v4(
                SignatureType::KeyBinding,
                subkey.algorithm(),
                HashAlgorithm::Sha256,
            );
            config.hashed_subpackets = vec![
                Subpacket::regular(SubpacketData::SignatureCreationTime(Timestamp::from_secs(
                    binding.created,
                )))
                .expect("subpacket"),
                Subpacket::regular(SubpacketData::IssuerFingerprint(subkey.fingerprint()))
                    .expect("subpacket"),
            ];
            let backsig = config
                .sign_primary_key_binding(
                    subkey,
                    subkey.public_key(),
                    &Password::empty(),
                    key.primary_key.public_key(),
                )
                .expect("back-signature");
            more.push(SubpacketData::EmbeddedSignature(Box::new(backsig)));
        }
        let mut config = SignatureConfig::v4(
            SignatureType::SubkeyBinding,
            signer.algorithm(),
            HashAlgorithm::Sha256,
        );
        config.hashed_subpackets = hashed(key, binding.created, more);
        config
            .sign_subkey_binding(
                signer,
                key.primary_key.public_key(),
                &Password::empty(),
                subkey.public_key(),
            )
            .expect("binding")
    }

    /// A revocation of `subkey` made `days_ago` by `key`'s primary key.
    pub(crate) fn subkey_revocation(
        key: &SignedSecretKey,
        subkey: &SecretSubkey,
        days_ago: u32,
    ) -> Signature {
        let mut config = SignatureConfig::v4(
            SignatureType::SubkeyRevocation,
            key.primary_key.algorithm(),
            HashAlgorithm::Sha256,
        );
        config.hashed_subpackets = hashed(
            key,
            now() - days_ago * DAY,
            vec![SubpacketData::RevocationReason(
                RevocationCode::KeyRetired,
                "retired".into(),
            )],
        );
        config
            .sign_subkey_binding(
                &key.primary_key,
                key.primary_key.public_key(),
                &Password::empty(),
                subkey.public_key(),
            )
            .expect("revocation")
    }

    /// A revocation of `key`'s primary key, made a day ago.
    pub(crate) fn key_revocation(key: &SignedSecretKey) -> Signature {
        let mut config = SignatureConfig::v4(
            SignatureType::KeyRevocation,
            key.primary_key.algorithm(),
            HashAlgorithm::Sha256,
        );
        config.hashed_subpackets = hashed(
            key,
            now() - DAY,
            vec![SubpacketData::RevocationReason(
                RevocationCode::KeySuperseded,
                "superseded".into(),
            )],
        );
        config
            .sign_key(
                &key.primary_key,
                &Password::empty(),
                key.primary_key.public_key(),
            )
            .expect("revocation")
    }

    /// A positive self-certification of `key`'s first user ID signed by
    /// `signer` (any key other than the primary forges it), with the key
    /// flags of a Proton primary key and an optional key expiration.
    pub(crate) fn user_certification(
        key: &SignedSecretKey,
        signer: &impl SigningKey,
        expires_after: Option<u32>,
    ) -> Signature {
        let mut flags = KeyFlags::default();
        flags.set_certify(true);
        flags.set_sign(true);
        let mut more = vec![SubpacketData::KeyFlags(flags)];
        if let Some(seconds) = expires_after {
            more.push(SubpacketData::KeyExpirationTime(Duration::from_secs(
                seconds,
            )));
        }
        let mut config = SignatureConfig::v4(
            SignatureType::CertPositive,
            signer.algorithm(),
            HashAlgorithm::Sha256,
        );
        config.hashed_subpackets = hashed(key, now() - DAY, more);
        config
            .sign_certification_third_party(
                signer,
                &Password::empty(),
                key.primary_key.public_key(),
                Tag::UserId,
                &key.details.users[0].id,
            )
            .expect("certification")
    }

    /// Replaces subkey `index`'s signatures.
    pub(crate) fn set_signatures(
        key: &mut SignedSecretKey,
        index: usize,
        signatures: Vec<Signature>,
    ) {
        key.secret_subkeys[index].signatures = signatures;
    }

    /// Subkey `index`, for signing bindings over it.
    pub(crate) fn subkey(key: &SignedSecretKey, index: usize) -> SecretSubkey {
        key.secret_subkeys[index].key.clone()
    }

    /// Adds `subkey` of `other` to `key` as a secret subkey with
    /// `signatures`, as a server could.
    pub(crate) fn inject_secret(
        key: &mut SignedSecretKey,
        subkey: SecretSubkey,
        signatures: Vec<Signature>,
    ) {
        key.secret_subkeys.push(SignedSecretSubKey {
            key: subkey,
            signatures,
        });
    }

    /// The legacy key ID of subkey `index`, lower-case hex, to compare
    /// with a key packet's recipient.
    pub(crate) fn subkey_id(key: &SignedSecretKey, index: usize) -> String {
        key_id_hex(&key.secret_subkeys[index].key)
    }

    pub(crate) fn key_id_hex(key: &impl KeyDetails) -> String {
        hex_id(&key.legacy_key_id())
    }

    /// The recipient key IDs of the key packets in a binary or armored
    /// OpenPGP message, lower-case hex.
    pub(crate) fn recipients(message: &[u8]) -> Vec<String> {
        use std::io::{BufReader, Read};

        use pgp::packet::{Packet, PacketParser};

        let mut bytes = message.to_vec();
        if message.starts_with(b"-----BEGIN") {
            bytes.clear();
            pgp::armor::Dearmor::new(BufReader::new(message))
                .read_to_end(&mut bytes)
                .expect("dearmor");
        }
        PacketParser::new(BufReader::new(bytes.as_slice()))
            .filter_map(|packet| match packet {
                Ok(Packet::PublicKeyEncryptedSessionKey(pkesk)) => {
                    Some(hex_id(pkesk.id().expect("v3 PKESK")))
                }
                _ => None,
            })
            .collect()
    }

    fn hex_id(id: &pgp::types::KeyId) -> String {
        id.as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
