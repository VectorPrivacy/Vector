//! MDK Key Packages

use mdk_storage_traits::MdkStorageProvider;
use mdk_storage_traits::mls_codec::MlsCodec;
use nostr::{Event, Kind, PublicKey, RelayUrl, Tag, TagKind};
use openmls::ciphersuite::hash_ref::HashReference;
use openmls::key_packages::KeyPackage;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::MDK;
use crate::constant::{DEFAULT_CIPHERSUITE, TAG_EXTENSIONS};
use crate::error::Error;
use crate::util::{ContentEncoding, NostrTagFormat, decode_content, encode_content};

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Creates a key package for a Nostr event.
    ///
    /// This function generates an encoded key package that is used as the content field of a kind:443 Nostr event.
    /// The encoding format is always base64 with an explicit `["encoding", "base64"]` tag per MIP-00/MIP-02.
    /// This prevents downgrade attacks and parsing ambiguity across clients.
    ///
    /// The key package contains the user's credential and capabilities required for MLS operations.
    ///
    /// **Note**: This function does NOT add the NIP-70 protected tag, ensuring maximum relay
    /// compatibility. Many popular relays (Damus, Primal, nos.lol) reject protected events.
    /// If you need the protected tag, use `create_key_package_for_event_with_options` instead.
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// * A base64-encoded string containing the serialized key package
    /// * A vector of tags for the Nostr event:
    ///   - `mls_protocol_version` - MLS protocol version (e.g., "1.0")
    ///   - `mls_ciphersuite` - Ciphersuite identifier (e.g., "0x0001")
    ///   - `mls_extensions` - Required MLS extensions
    ///   - `relays` - Relay URLs for distribution
    ///   - `i` - Hex-encoded KeyPackageRef for efficient relay queries (MIP-00)
    ///   - `client` - Client identifier and version
    ///   - `encoding` - The encoding format tag ("base64")
    /// * The serialized hash_ref bytes for the key package (for lifecycle tracking)
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// * It fails to generate the credential and signature keypair
    /// * It fails to build the key package
    /// * It fails to serialize the key package
    pub fn create_key_package_for_event<I>(
        &self,
        public_key: &PublicKey,
        relays: I,
    ) -> Result<(String, Vec<Tag>, Vec<u8>), Error>
    where
        I: IntoIterator<Item = RelayUrl>,
    {
        self.create_key_package_for_event_internal(public_key, relays, false)
    }

    /// Creates a key package for a Nostr event with additional options.
    ///
    /// This is the same as `create_key_package_for_event` but allows specifying
    /// whether to include the NIP-70 protected tag.
    ///
    /// # Arguments
    ///
    /// * `public_key` - The Nostr public key for the credential
    /// * `relays` - Relay URLs where the key package will be published
    /// * `protected` - Whether to add the NIP-70 protected tag (`["-"]`). When `true`, relays
    ///   that implement NIP-70 will reject republishing by third parties. However, many popular
    ///   relays (Damus, Primal, nos.lol) reject protected events entirely. Only set to `true`
    ///   if publishing to relays known to accept NIP-70 protected events.
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// * A base64-encoded string containing the serialized key package
    /// * A vector of tags for the Nostr event
    /// * The serialized hash_ref bytes for the key package (for lifecycle tracking)
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// * It fails to generate the credential and signature keypair
    /// * It fails to build the key package
    /// * It fails to serialize the key package
    pub fn create_key_package_for_event_with_options<I>(
        &self,
        public_key: &PublicKey,
        relays: I,
        protected: bool,
    ) -> Result<(String, Vec<Tag>, Vec<u8>), Error>
    where
        I: IntoIterator<Item = RelayUrl>,
    {
        self.create_key_package_for_event_internal(public_key, relays, protected)
    }

    /// Internal implementation for creating key packages.
    ///
    /// This is the shared implementation used by both `create_key_package_for_event` and
    /// `create_key_package_for_event_with_options`. It generates an MLS key package with
    /// the user's credential and builds the Nostr event tags.
    ///
    /// The `protected` parameter controls whether the NIP-70 protected tag (`["-"]`) is
    /// included in the output tags. When `true`, the tag is inserted between the `i`
    /// and `client` tags, resulting in 8 total tags. When `false`, the protected tag is
    /// omitted, resulting in 7 total tags.
    fn create_key_package_for_event_internal<I>(
        &self,
        public_key: &PublicKey,
        relays: I,
        protected: bool,
    ) -> Result<(String, Vec<Tag>, Vec<u8>), Error>
    where
        I: IntoIterator<Item = RelayUrl>,
    {
        let (credential, signature_keypair) = self.generate_credential_with_key(public_key)?;

        let capabilities: Capabilities = self.capabilities();

        let key_package_bundle = KeyPackage::builder()
            .leaf_node_capabilities(capabilities)
            .mark_as_last_resort()
            .build(
                self.ciphersuite,
                &self.provider,
                &signature_keypair,
                credential,
            )?;

        // Compute hash_ref while we have the KeyPackage available.
        // This allows callers to track the key package for later cleanup
        // without needing to re-parse it.
        let hash_ref = key_package_bundle
            .key_package()
            .hash_ref(self.provider.crypto())?;
        let hash_ref_bytes = MlsCodec::serialize(&hash_ref)
            .map_err(|e| Error::Provider(format!("Failed to serialize hash_ref: {}", e)))?;

        let key_package_serialized = key_package_bundle.key_package().tls_serialize_detached()?;

        // SECURITY: Always use base64 encoding with explicit encoding tag per MIP-00/MIP-02.
        // This prevents downgrade attacks and parsing ambiguity across clients.
        let encoding = ContentEncoding::Base64;

        let encoded_content = encode_content(&key_package_serialized, encoding);

        tracing::debug!(
            target: "mdk_core::key_packages",
            "Encoded key package using {} format (protected: {})",
            encoding.as_tag_value(),
            protected
        );

        // Hex-encode the raw KeyPackageRef bytes for the `i` tag per MIP-00.
        // This enables efficient relay queries by KeyPackageRef without downloading
        // and decoding all KeyPackage events.
        let key_package_ref_hex = hex::encode(hash_ref.as_slice());

        let mut tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, [self.ciphersuite_value()]),
            Tag::custom(TagKind::MlsExtensions, self.extensions_value()),
            Tag::relays(relays),
            Tag::custom(TagKind::i(), [key_package_ref_hex]),
        ];

        if protected {
            tags.push(Tag::protected());
        }

        tags.push(Tag::client(format!("MDK/{}", env!("CARGO_PKG_VERSION"))));
        tags.push(Tag::custom(
            TagKind::Custom("encoding".into()),
            [encoding.as_tag_value()],
        ));

        Ok((encoded_content, tags, hash_ref_bytes))
    }

    /// Parses and validates a key package using base64 encoding.
    ///
    /// # Arguments
    ///
    /// * `key_package_str` - A base64-encoded string containing the serialized key package
    /// * `encoding` - The encoding format (must be Base64)
    ///
    /// # Returns
    ///
    /// A validated KeyPackage on success, or an Error on failure.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// * The specified encoding format fails to decode
    /// * The TLS deserialization fails
    /// * The key package validation fails (invalid signature, ciphersuite, or extensions)
    fn parse_serialized_key_package(
        &self,
        key_package_str: &str,
        encoding: ContentEncoding,
    ) -> Result<KeyPackage, Error> {
        let (key_package_bytes, format) =
            decode_content(key_package_str, encoding, "key package").map_err(Error::KeyPackage)?;

        tracing::debug!(
            target: "mdk_core::key_packages",
            "Decoded key package using {}", format
        );

        let key_package_in = KeyPackageIn::tls_deserialize(&mut key_package_bytes.as_slice())?;

        let key_package =
            key_package_in.validate(self.provider.crypto(), ProtocolVersion::Mls10)?;

        Ok(key_package)
    }

    /// Parses and validates an MLS KeyPackage from a Nostr event.
    ///
    /// This method performs comprehensive validation:
    /// 1. Verifies the event is of kind `MlsKeyPackage` (Kind 443)
    /// 2. Validates all required tags are present and correctly formatted per MIP-00:
    ///    - `mls_protocol_version`: Protocol version (e.g., "1.0")
    ///    - `mls_ciphersuite`: Must be "0x0001" (MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519)
    ///    - `mls_extensions`: Must include all required extensions (0x000a, 0xf2ee), but not the default extensions (0x0003, 0x0002)
    ///    - `i`: Hex-encoded KeyPackageRef, validated against computed value from content
    /// 3. Deserializes the TLS-encoded key package from the event content
    /// 4. Verifies identity binding (credential identity matches event signer)
    /// 5. Validates the `i` tag value matches the computed `KeyPackageRef` from the parsed content
    ///
    /// # Arguments
    ///
    /// * `event` - A Nostr event of kind `MlsKeyPackage` containing the serialized key package
    ///
    /// # Returns
    ///
    /// * `Ok(KeyPackage)` - Successfully parsed and validated key package
    /// * `Err(Error::UnexpectedEvent)` - Event is not of kind `MlsKeyPackage`
    /// * `Err(Error::KeyPackage)` - Tag validation failed (missing tags, invalid format, or unsupported values)
    /// * `Err(Error)` - Deserialization failed (malformed TLS data)
    ///
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use mdk_core::MDK;
    /// # use nostr::Event;
    /// # fn example(mdk: &MDK<impl mdk_storage_traits::MdkStorageProvider>, event: &Event) -> Result<(), Box<dyn std::error::Error>> {
    /// // Parse key package from a received Nostr event
    /// let key_package = mdk.parse_key_package(event)?;
    ///
    /// // Key package is now validated and ready to use for MLS operations
    /// println!("Parsed key package with cipher suite: {:?}", key_package.ciphersuite());
    /// # Ok(())
    /// # }
    /// ```
    pub fn parse_key_package(&self, event: &Event) -> Result<KeyPackage, Error> {
        if event.kind != Kind::MlsKeyPackage {
            return Err(Error::UnexpectedEvent {
                expected: Kind::MlsKeyPackage,
                received: event.kind,
            });
        }

        // Validate tag format before parsing the key package content.
        // This catches malformed tags early without the cost of deserialization.
        self.validate_key_package_tags(event, None)?;

        // SECURITY: Require explicit encoding tag to prevent downgrade attacks and parsing ambiguity.
        // Per MIP-00/MIP-02, encoding tag must be present.
        let encoding = ContentEncoding::from_tags(event.tags.iter())
            .ok_or_else(|| Error::KeyPackage("Missing required encoding tag".to_string()))?;

        let key_package = self.parse_serialized_key_package(&event.content, encoding)?;

        // SECURITY: Verify identity binding between the event signer and the credential identity.
        // This prevents an attacker from publishing a kind-443 event with a KeyPackage whose
        // BasicCredential.identity claims a victim's Nostr public key while signing with their own key.
        // Without this check, the attacker could join groups appearing as the victim and potentially
        // gain admin privileges if the victim is an admin.
        let credential = BasicCredential::try_from(key_package.leaf_node().credential().clone())?;
        let credential_identity = self.parse_credential_identity(credential.identity())?;

        if credential_identity != event.pubkey {
            return Err(Error::KeyPackageIdentityMismatch {
                credential_identity: credential_identity.to_hex(),
                event_signer: event.pubkey.to_hex(),
            });
        }

        // SECURITY: Validate that the `i` tag value matches the computed KeyPackageRef.
        // Now that we have the parsed key package, re-validate tags with content verification.
        self.validate_key_package_tags(event, Some(&key_package))?;

        Ok(key_package)
    }

    /// Validates that key package event tags match MIP-00 specification.
    ///
    /// This function checks that:
    /// - The event has the required tags (mls_protocol_version, mls_ciphersuite, mls_extensions, relays, i)
    /// - Tag values are in the correct format and contain valid values
    /// - The relays tag contains at least one valid relay URL (mandatory per MIP-00)
    /// - The `i` tag contains valid hex-encoded data (mandatory per MIP-00)
    /// - If a parsed `KeyPackage` is provided, the `i` tag value matches the computed `KeyPackageRef`
    /// - Supports backward compatibility with legacy formats
    ///
    /// # Arguments
    ///
    /// * `event` - The key package event to validate
    /// * `key_package` - Optional parsed key package for content-level `i` tag verification
    ///
    /// # Returns
    ///
    /// Ok(()) if validation succeeds, or an Error describing what's wrong
    fn validate_key_package_tags(
        &self,
        event: &Event,
        key_package: Option<&KeyPackage>,
    ) -> Result<(), Error> {
        let require = |pred: fn(&Self, &Tag) -> bool, name: &str| {
            event
                .tags
                .iter()
                .find(|t| pred(self, t))
                .ok_or_else(|| Error::KeyPackage(format!("Missing required tag: {}", name)))
        };

        let pv = require(Self::is_protocol_version_tag, "mls_protocol_version")?;
        let cs = require(Self::is_ciphersuite_tag, "mls_ciphersuite")?;
        let ext = require(Self::is_extensions_tag, "mls_extensions")?;
        let relays = require(Self::is_relays_tag, "relays")?;
        let i_tag = require(Self::is_key_package_ref_tag, "i")?;

        self.validate_protocol_version_tag(pv)?;
        self.validate_ciphersuite_tag(cs)?;
        self.validate_extensions_tag(ext)?;
        self.validate_relays_tag(relays)?;
        self.validate_key_package_ref_tag(i_tag)?;

        // SECURITY: When a parsed KeyPackage is available, validate that the `i` tag value
        // matches the computed KeyPackageRef. This prevents an attacker from publishing a
        // KeyPackage event with a fabricated `i` tag that doesn't correspond to the actual
        // key package content, which could cause relay query results to return incorrect
        // key packages.
        if let Some(kp) = key_package {
            let computed_ref = kp.hash_ref(self.provider.crypto())?;

            let i_tag_value = i_tag
                .as_slice()
                .get(1)
                .ok_or_else(|| Error::KeyPackage("Missing required i tag".to_string()))?;

            let i_tag_bytes = hex::decode(i_tag_value.as_str())
                .map_err(|_| Error::KeyPackage("Invalid i tag hex".to_string()))?;

            if i_tag_bytes != computed_ref.as_slice() {
                return Err(Error::KeyPackage(
                    "KeyPackageRef in i tag does not match computed value from content".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Checks if a tag is a protocol version tag (MIP-00).
    ///
    /// **SPEC-COMPLIANT**: "mls_protocol_version"
    fn is_protocol_version_tag(&self, tag: &Tag) -> bool {
        matches!(tag.kind(), TagKind::MlsProtocolVersion)
    }

    /// Checks if a tag is a ciphersuite tag (MIP-00).
    ///
    /// **SPEC-COMPLIANT**: "mls_ciphersuite"
    fn is_ciphersuite_tag(&self, tag: &Tag) -> bool {
        matches!(tag.kind(), TagKind::MlsCiphersuite)
    }

    /// Checks if a tag is an extensions tag (MIP-00).
    ///
    /// **SPEC-COMPLIANT**: "mls_extensions"
    fn is_extensions_tag(&self, tag: &Tag) -> bool {
        matches!(tag.kind(), TagKind::MlsExtensions)
    }

    /// Checks if a tag is a relays tag (MIP-00).
    ///
    /// **SPEC-COMPLIANT**: "relays"
    fn is_relays_tag(&self, tag: &Tag) -> bool {
        matches!(tag.kind(), TagKind::Relays)
    }

    /// Checks if a tag is a KeyPackageRef `i` tag (MIP-00).
    ///
    /// **SPEC-COMPLIANT**: "i" tag containing hex-encoded KeyPackageRef
    fn is_key_package_ref_tag(&self, tag: &Tag) -> bool {
        tag.kind() == TagKind::i()
    }

    /// Validates protocol version tag format and value.
    ///
    /// **SPEC-COMPLIANT**: Per MIP-00, only "1.0" is currently supported.
    fn validate_protocol_version_tag(&self, tag: &Tag) -> Result<(), Error> {
        let values: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();

        // Skip the tag name (first element) and get the value
        let version_value = values.get(1).ok_or_else(|| {
            Error::KeyPackage("Protocol version tag must have a value".to_string())
        })?;

        // Validate the version value
        if *version_value != "1.0" {
            return Err(Error::KeyPackage(format!(
                "Unsupported protocol version: {}. Only version 1.0 is supported per MIP-00",
                version_value
            )));
        }

        Ok(())
    }

    /// Validates ciphersuite tag format and value per MIP-00.
    ///
    /// Currently only accepts: "0x0001" (MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519)
    fn validate_ciphersuite_tag(&self, tag: &Tag) -> Result<(), Error> {
        let values: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();

        // Skip the tag name (first element) and get the value
        let ciphersuite_value = values
            .get(1)
            .ok_or_else(|| Error::KeyPackage("Ciphersuite tag must have a value".to_string()))?;

        // Validate length
        if ciphersuite_value.len() != 6 {
            return Err(Error::KeyPackage(format!(
                "Ciphersuite hex value must be 6 characters (0xXXXX), got: {}",
                ciphersuite_value
            )));
        }

        // Verify format: "0x" prefix + 4 hex digits
        ciphersuite_value
            .strip_prefix("0x")
            .filter(|hex| hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| {
                Error::KeyPackage(format!(
                    "Ciphersuite value must be 0x followed by 4 hex digits, got: {}",
                    ciphersuite_value
                ))
            })?;

        // Validate the actual value - must match DEFAULT_CIPHERSUITE
        let expected_hex = DEFAULT_CIPHERSUITE.to_nostr_tag();
        if ciphersuite_value.to_lowercase() != expected_hex.to_lowercase() {
            return Err(Error::KeyPackage(format!(
                "Unsupported ciphersuite: {}. Only {} (MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519) is supported",
                ciphersuite_value, expected_hex
            )));
        }

        Ok(())
    }

    /// Validates extensions tag format and values per MIP-00.
    ///
    /// Required extensions (as separate hex values):
    /// - 0x000a (LastResort)
    /// - 0xf2ee (NostrGroupData)
    fn validate_extensions_tag(&self, tag: &Tag) -> Result<(), Error> {
        let values: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();

        // Skip the tag name (first element) and get extension values
        let extension_values: Vec<&str> = values.iter().skip(1).copied().collect();

        if extension_values.is_empty() {
            return Err(Error::KeyPackage(
                "Extensions tag must have at least one value".to_string(),
            ));
        }

        // Validate format of each hex value
        for (idx, ext_value) in extension_values.iter().enumerate() {
            // Validate length
            if ext_value.len() != 6 {
                return Err(Error::KeyPackage(format!(
                    "Extension {} hex value must be 6 characters (0xXXXX), got: {}",
                    idx, ext_value
                )));
            }

            // Verify format: "0x" prefix + 4 hex digits
            ext_value
                .strip_prefix("0x")
                .filter(|hex| hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()))
                .ok_or_else(|| {
                    Error::KeyPackage(format!(
                        "Extension {} value must be 0x followed by 4 hex digits, got: {}",
                        idx, ext_value
                    ))
                })?;
        }

        // Validate that all required extensions are present
        // Normalize extension values to lowercase for case-insensitive comparison
        let normalized_extensions: std::collections::HashSet<String> =
            extension_values.iter().map(|s| s.to_lowercase()).collect();

        for required_ext in TAG_EXTENSIONS.iter() {
            let required_hex = required_ext.to_nostr_tag();
            if !normalized_extensions.contains(&required_hex) {
                let ext_name = match u16::from(*required_ext) {
                    0x000a => "LastResort",
                    0xf2ee => "NostrGroupData",
                    _ => "Unknown",
                };
                return Err(Error::KeyPackage(format!(
                    "Missing required extension: {} ({})",
                    required_hex, ext_name
                )));
            }
        }

        Ok(())
    }

    /// Validates relays tag format and values.
    ///
    /// **SPEC-COMPLIANT**: Per MIP-00, the relays tag is mandatory and must contain
    /// at least one valid relay URL. This ensures key packages are routable.
    fn validate_relays_tag(&self, tag: &Tag) -> Result<(), Error> {
        let relay_slice = tag.as_slice();

        // Check that relays tag has at least one relay URL (first element is tag name)
        if relay_slice.len() <= 1 {
            return Err(Error::KeyPackage(
                "Relays tag must have at least one relay URL".to_string(),
            ));
        }

        // Validate that each relay URL is properly formatted and parses as a RelayUrl
        for (idx, relay_url_str) in relay_slice.iter().skip(1).enumerate() {
            RelayUrl::parse(relay_url_str).map_err(|e| {
                Error::KeyPackage(format!(
                    "Invalid relay URL at index {}: {} ({})",
                    idx, relay_url_str, e
                ))
            })?;
        }

        Ok(())
    }

    /// Validates the `i` tag format per MIP-00.
    ///
    /// The `i` tag must contain a single hex-encoded `KeyPackageRef` value.
    /// Content validation (matching the computed `KeyPackageRef`) is performed
    /// by `validate_key_package_tags` when a parsed `KeyPackage` is provided.
    fn validate_key_package_ref_tag(&self, tag: &Tag) -> Result<(), Error> {
        let slice = tag.as_slice();

        // Tag slice includes the tag name at index 0, so exactly 2 elements means one value.
        if slice.len() != 2 {
            return Err(Error::KeyPackage(
                "i tag must contain exactly one value".to_string(),
            ));
        }

        let hex_value = &slice[1];

        if hex_value.is_empty() {
            return Err(Error::KeyPackage(
                "i tag value must not be empty".to_string(),
            ));
        }

        // Validate that the value is valid hex
        hex::decode(hex_value.as_str()).map_err(|e| {
            Error::KeyPackage(format!("i tag must contain valid hex-encoded data: {}", e))
        })?;

        Ok(())
    }

    /// Deletes a key package from the MLS provider's storage.
    /// TODO: Do we need to delete the encryption keys from the MLS storage provider?
    ///
    /// # Arguments
    ///
    /// * `key_package` - The key package to delete
    pub fn delete_key_package_from_storage(&self, key_package: &KeyPackage) -> Result<(), Error> {
        let hash_ref = key_package.hash_ref(self.provider.crypto())?;

        self.provider
            .storage()
            .delete_key_package(&hash_ref)
            .map_err(|e| Error::Provider(e.to_string()))?;

        Ok(())
    }

    /// Deletes a key package from storage using previously serialized hash_ref bytes.
    ///
    /// The `hash_ref_bytes` should be the bytes returned as the third element of
    /// [`create_key_package_for_event`](Self::create_key_package_for_event).
    pub fn delete_key_package_from_storage_by_hash_ref(
        &self,
        hash_ref_bytes: &[u8],
    ) -> Result<(), Error> {
        let hash_ref: HashReference = MlsCodec::deserialize(hash_ref_bytes)
            .map_err(|e| Error::Provider(format!("Failed to deserialize hash_ref: {}", e)))?;

        self.provider
            .storage()
            .delete_key_package(&hash_ref)
            .map_err(|e| Error::Provider(e.to_string()))?;

        Ok(())
    }

    /// Generates a credential with a key for MLS (Messaging Layer Security) operations.
    ///
    /// This function creates a new credential and associated signature key pair for use in MLS.
    /// It uses the default MDK configuration and stores the generated key pair in the
    /// crypto provider's storage.
    ///
    /// # Arguments
    ///
    /// * `pubkey` - The user's nostr pubkey
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// * `CredentialWithKey` - The generated credential along with its public key.
    /// * `SignatureKeyPair` - The generated signature key pair.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// * It fails to generate a signature key pair.
    /// * It fails to store the signature key pair in the crypto provider's storage.
    pub(crate) fn generate_credential_with_key(
        &self,
        public_key: &PublicKey,
    ) -> Result<(CredentialWithKey, SignatureKeyPair), Error> {
        let public_key_bytes: Vec<u8> = public_key.to_bytes().to_vec();

        let credential = BasicCredential::new(public_key_bytes);
        let signature_keypair = SignatureKeyPair::new(self.ciphersuite.signature_algorithm())?;

        signature_keypair
            .store(self.provider.storage())
            .map_err(|e| Error::Provider(e.to_string()))?;

        Ok((
            CredentialWithKey {
                credential: credential.into(),
                signature_key: signature_keypair.public().into(),
            },
            signature_keypair,
        ))
    }

    /// Parses a public key from credential identity bytes.
    ///
    /// Per MIP-00, the credential identity must be exactly 32 bytes containing
    /// the raw Nostr public key.
    ///
    /// # Arguments
    ///
    /// * `identity_bytes` - The raw bytes from a BasicCredential's identity field
    ///
    /// # Returns
    ///
    /// * `Ok(PublicKey)` - The parsed public key
    /// * `Err(Error)` - If the identity bytes are not exactly 32 bytes or invalid
    pub(crate) fn parse_credential_identity(
        &self,
        identity_bytes: &[u8],
    ) -> Result<PublicKey, Error> {
        if identity_bytes.len() != 32 {
            return Err(Error::KeyPackage(format!(
                "Invalid credential identity length: {} (expected 32)",
                identity_bytes.len()
            )));
        }

        PublicKey::from_slice(identity_bytes)
            .map_err(|e| Error::KeyPackage(format!("Invalid public key: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use nostr::EventBuilder;
    use nostr::Keys;
    use nostr::base64::Engine;
    use nostr::base64::engine::general_purpose::STANDARD as BASE64;

    use super::*;
    use crate::constant::DEFAULT_CIPHERSUITE;
    use crate::test_util::create_nostr_group_config_data;
    use crate::tests::create_test_mdk;

    #[test]
    fn test_key_package_creation_and_parsing() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        // Create key package without protected tag (default for maximum relay compatibility)
        let (key_package_str, tags, _hash_ref) = mdk
            .create_key_package_for_event(&test_pubkey, relays.clone())
            .expect("Failed to create key package");

        // Create new instance for parsing
        let parsing_mls = create_test_mdk();

        // Parse and validate the key package
        let key_package = parsing_mls
            .parse_serialized_key_package(&key_package_str, ContentEncoding::Base64)
            .expect("Failed to parse key package");

        // Verify the key package has the expected properties
        assert_eq!(key_package.ciphersuite(), DEFAULT_CIPHERSUITE);

        // Without protected tag: 7 tags (3 MLS + relays + i + client + encoding)
        assert_eq!(tags.len(), 7);
        assert_eq!(tags[0].kind(), TagKind::MlsProtocolVersion);
        assert_eq!(tags[1].kind(), TagKind::MlsCiphersuite);
        assert_eq!(tags[2].kind(), TagKind::MlsExtensions);
        assert_eq!(tags[3].kind(), TagKind::Relays);
        assert_eq!(tags[4].kind(), TagKind::i());
        assert_eq!(tags[5].kind(), TagKind::Client);
        assert_eq!(tags[6].kind(), TagKind::Custom("encoding".into()));

        assert_eq!(
            tags[3].content().unwrap(),
            relays
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        // Verify protected tag is NOT present when protected=false
        assert!(
            !tags.iter().any(|t| t.kind() == TagKind::Protected),
            "Protected tag should not be present when protected=false"
        );

        // Verify client tag contains version
        let client_tag = tags[5].content().unwrap();
        assert!(
            client_tag.starts_with("MDK/"),
            "Client tag should start with MDK/"
        );
        assert!(
            client_tag.contains('.'),
            "Client tag should contain version number"
        );
    }

    /// Test that ciphersuite tag format matches Marmot spec (MIP-00)
    /// Spec requires: ["ciphersuite", "0x0001"]
    #[test]
    fn test_ciphersuite_tag_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (_, tags, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create key package");

        // Find ciphersuite tag
        let ciphersuite_tag = tags
            .iter()
            .find(|t| t.kind() == TagKind::MlsCiphersuite)
            .expect("Ciphersuite tag not found");

        // Verify format: should be hex with 0x prefix
        let ciphersuite_value = ciphersuite_tag.content().unwrap();
        assert!(
            ciphersuite_value.starts_with("0x"),
            "Ciphersuite value should start with '0x', got: {}",
            ciphersuite_value
        );

        // For DEFAULT_CIPHERSUITE (MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519), value is 0x0001
        assert_eq!(
            ciphersuite_value, "0x0001",
            "Expected ciphersuite '0x0001' per MIP-00 spec, got: {}",
            ciphersuite_value
        );
    }

    /// Test that extensions tag format matches Marmot spec (MIP-00)
    /// Spec requires: ["extensions", "0x0001", "0x0002", "0x0003", ...]
    /// Each extension ID should be a separate hex value with 0x prefix
    #[test]
    fn test_extensions_tag_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (_, tags, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create key package");

        // Find extensions tag
        let extensions_tag = tags
            .iter()
            .find(|t| t.kind() == TagKind::MlsExtensions)
            .expect("Extensions tag not found");

        // Get all values (first value is the tag name "mls_extensions", rest are extension IDs)
        let tag_values: Vec<String> = extensions_tag
            .as_slice()
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Should have at least 3 elements: tag name + 2 extension IDs (0x000a, 0xf2ee)
        assert!(
            tag_values.len() >= 3,
            "Expected at least 3 values (tag name + 2 extensions), got: {}",
            tag_values.len()
        );

        // Skip first element (tag name) and verify all extension IDs are hex format
        let extension_ids = &tag_values[1..];
        for (i, ext_id) in extension_ids.iter().enumerate() {
            assert!(
                ext_id.starts_with("0x"),
                "Extension ID {} should start with '0x', got: {}",
                i,
                ext_id
            );
            assert!(
                ext_id.len() == 6, // "0x" + 4 hex digits
                "Extension ID {} should be 6 chars (0xXXXX), got: {} with length {}",
                i,
                ext_id,
                ext_id.len()
            );
        }

        // Verify expected non-default extension IDs are present in tags
        // Tags must match the KeyPackage capabilities to allow other clients to
        // validate compatibility. Per RFC 9420 Section 7.2, only non-default
        // extensions need to be listed in capabilities.
        //
        // We advertise:
        // - 0x000a = LastResort (KeyPackage extension, required in capabilities by OpenMLS)
        // - 0xf2ee = NostrGroupData (custom GroupContext extension)
        //
        // Default extensions (RequiredCapabilities, RatchetTree, etc.) are assumed
        // supported and should NOT be listed per RFC 9420 Section 7.2.
        assert!(
            extension_ids.contains(&"0x000a".to_string()),
            "Should contain LastResort (0x000a)"
        );
        assert!(
            extension_ids.contains(&"0xf2ee".to_string()),
            "Should contain NostrGroupData (0xf2ee)"
        );

        // Verify we have exactly 2 non-default extensions in tags
        assert_eq!(
            extension_ids.len(),
            2,
            "Should have 2 extensions in tags (0x000a, 0xf2ee), found: {:?}",
            extension_ids
        );
    }

    /// Test that protocol version tag matches Marmot spec (MIP-00)
    /// Spec requires: ["mls_protocol_version", "1.0"]
    #[test]
    fn test_protocol_version_tag_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (_, tags, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create key package");

        // Find protocol version tag
        let version_tag = tags
            .iter()
            .find(|t| t.kind() == TagKind::MlsProtocolVersion)
            .expect("Protocol version tag not found");

        let version_value = version_tag.content().unwrap();
        assert_eq!(
            version_value, "1.0",
            "Expected protocol version '1.0' per MIP-00 spec, got: {}",
            version_value
        );
    }

    /// Test complete tag structure matches Marmot spec (MIP-00) without protected tag
    /// This is an integration test ensuring all tags work together correctly
    #[test]
    fn test_complete_tag_structure_mip00_compliance_without_protected() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![
            RelayUrl::parse("wss://relay1.example.com").unwrap(),
            RelayUrl::parse("wss://relay2.example.com").unwrap(),
        ];

        let (_, tags, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays.clone())
            .expect("Failed to create key package");

        // Verify we have exactly 7 tags (3 MLS required + relays + i + client + encoding)
        // No protected tag when protected=false
        assert_eq!(
            tags.len(),
            7,
            "Should have exactly 7 tags without protected"
        );

        // Verify tag order matches spec example
        assert_eq!(
            tags[0].kind(),
            TagKind::MlsProtocolVersion,
            "First tag should be mls_protocol_version"
        );
        assert_eq!(
            tags[1].kind(),
            TagKind::MlsCiphersuite,
            "Second tag should be mls_ciphersuite"
        );
        assert_eq!(
            tags[2].kind(),
            TagKind::MlsExtensions,
            "Third tag should be mls_extensions"
        );
        assert_eq!(
            tags[3].kind(),
            TagKind::Relays,
            "Fourth tag should be relays"
        );
        assert_eq!(
            tags[4].kind(),
            TagKind::i(),
            "Fifth tag should be i (KeyPackageRef)"
        );
        assert_eq!(
            tags[5].kind(),
            TagKind::Client,
            "Sixth tag should be client (no protected tag)"
        );
        assert_eq!(
            tags[6].kind(),
            TagKind::Custom("encoding".into()),
            "Seventh tag should be encoding"
        );

        // Verify relays tag format
        let relays_tag = &tags[3];
        let relays_values: Vec<String> = relays_tag
            .as_slice()
            .iter()
            .skip(1) // Skip tag name "relays"
            .map(|s| s.to_string())
            .collect();

        assert_eq!(relays_values.len(), 2, "Should have exactly 2 relay URLs");
        assert!(
            relays_values.contains(&"wss://relay1.example.com".to_string()),
            "Should contain relay1"
        );
        assert!(
            relays_values.contains(&"wss://relay2.example.com".to_string()),
            "Should contain relay2"
        );

        // Verify protected tag is NOT present
        assert!(
            !tags.iter().any(|t| t.kind() == TagKind::Protected),
            "Protected tag should not be present when protected=false"
        );
    }

    /// Test complete tag structure with protected tag (NIP-70)
    #[test]
    fn test_complete_tag_structure_with_protected() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (_, tags, hash_ref) = mdk
            .create_key_package_for_event_with_options(&test_pubkey, relays, true)
            .expect("Failed to create key package");

        // Verify hash_ref is returned from the with_options variant too
        assert!(
            !hash_ref.is_empty(),
            "hash_ref should be returned from create_key_package_for_event_with_options"
        );

        // Verify we have exactly 8 tags (3 MLS required + relays + i + protected + client + encoding)
        assert_eq!(
            tags.len(),
            8,
            "Should have exactly 8 tags with protected=true"
        );

        // Verify i tag is present at the correct position
        assert_eq!(
            tags[4].kind(),
            TagKind::i(),
            "Fifth tag should be i (KeyPackageRef)"
        );
        // Verify protected tag is present at the correct position
        assert_eq!(
            tags[5].kind(),
            TagKind::Protected,
            "Sixth tag should be protected"
        );
        assert_eq!(
            tags[6].kind(),
            TagKind::Client,
            "Seventh tag should be client"
        );
        assert_eq!(
            tags[7].kind(),
            TagKind::Custom("encoding".into()),
            "Eighth tag should be encoding"
        );
    }

    #[test]
    fn test_key_package_deletion() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        // Create and parse key package
        let (key_package_str, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays.clone())
            .expect("Failed to create key package");

        // Create new instance for parsing and deletion
        let deletion_mls = create_test_mdk();
        let key_package = deletion_mls
            .parse_serialized_key_package(&key_package_str, ContentEncoding::Base64)
            .expect("Failed to parse key package");

        // Delete the key package
        deletion_mls
            .delete_key_package_from_storage(&key_package)
            .expect("Failed to delete key package");
    }

    #[test]
    fn test_key_package_deletion_by_hash_ref() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        // hash_ref is returned directly from create_key_package_for_event
        let (_, _, hash_ref) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create key package");

        assert!(!hash_ref.is_empty(), "hash_ref bytes should not be empty");

        // Delete using the hash_ref bytes (simulates delayed cleanup)
        mdk.delete_key_package_from_storage_by_hash_ref(&hash_ref)
            .expect("Failed to delete key package by hash_ref");

        // Deleting again should also succeed (idempotent, no-op)
        mdk.delete_key_package_from_storage_by_hash_ref(&hash_ref)
            .expect("Second deletion should succeed (idempotent)");
    }

    #[test]
    fn test_invalid_key_package_parsing() {
        let mdk = create_test_mdk();

        // Try to parse invalid base64 encoding
        let result = mdk.parse_serialized_key_package("invalid!@#$%", ContentEncoding::Base64);
        assert!(
            matches!(result, Err(Error::KeyPackage(_))),
            "Should return KeyPackage error for invalid base64 encoding"
        );

        // Try to parse valid base64 but invalid key package
        let result = mdk.parse_serialized_key_package("YWJjZGVm", ContentEncoding::Base64);
        assert!(matches!(result, Err(Error::Tls(..))));
    }

    #[test]
    fn test_credential_generation() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let result = mdk.generate_credential_with_key(&test_pubkey);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_credential_identity() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        // Test correct format: 32-byte raw format per MIP-00
        let raw_bytes = test_pubkey.to_bytes();
        assert_eq!(raw_bytes.len(), 32, "Raw public key should be 32 bytes");

        let parsed = mdk
            .parse_credential_identity(&raw_bytes)
            .expect("Should parse 32-byte raw format");
        assert_eq!(
            parsed, test_pubkey,
            "Parsed public key from raw bytes should match original"
        );

        // Test that legacy 64-byte format is now rejected
        let hex_string = test_pubkey.to_hex();
        let utf8_bytes = hex_string.as_bytes();
        assert_eq!(utf8_bytes.len(), 64);

        let result = mdk.parse_credential_identity(utf8_bytes);
        assert!(
            matches!(result, Err(Error::KeyPackage(_))),
            "Should reject 64-byte legacy format"
        );

        // Test other invalid lengths
        let invalid_33_bytes = vec![0u8; 33];
        let result = mdk.parse_credential_identity(&invalid_33_bytes);
        assert!(
            matches!(result, Err(Error::KeyPackage(_))),
            "Should reject 33-byte input"
        );

        let invalid_31_bytes = vec![0u8; 31];
        let result = mdk.parse_credential_identity(&invalid_31_bytes);
        assert!(
            matches!(result, Err(Error::KeyPackage(_))),
            "Should reject 31-byte input"
        );
    }

    #[test]
    fn test_new_credentials_use_32_byte_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        // Generate a credential
        let (credential_with_key, _) = mdk
            .generate_credential_with_key(&test_pubkey)
            .expect("Should generate credential");

        // Extract the identity bytes
        let basic_credential = BasicCredential::try_from(credential_with_key.credential)
            .expect("Should extract basic credential");
        let identity_bytes = basic_credential.identity();

        // Verify it's using the new 32-byte format
        assert_eq!(
            identity_bytes.len(),
            32,
            "New credentials should use 32-byte raw format, not 64-byte UTF-8 encoded hex"
        );

        // Verify it's actually the raw bytes, not UTF-8 encoded hex
        let raw_bytes = test_pubkey.to_bytes();
        assert_eq!(
            identity_bytes, raw_bytes,
            "Identity should be raw public key bytes"
        );

        // Verify it's NOT the UTF-8 encoded hex string
        let hex_string = test_pubkey.to_hex();
        let utf8_bytes = hex_string.as_bytes();
        assert_ne!(
            identity_bytes, utf8_bytes,
            "Identity should NOT be UTF-8 encoded hex string"
        );
    }

    /// Test that missing required tags are rejected
    #[test]
    fn test_validate_missing_required_tags() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test missing protocol version
        {
            let tags = vec![
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0x000a"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event without protocol_version tag"
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("mls_protocol_version")
            );
        }

        // Test missing ciphersuite
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0x000a"]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event without ciphersuite tag"
            );
            assert!(result.unwrap_err().to_string().contains("mls_ciphersuite"));
        }

        // Test missing extensions
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event without extensions tag"
            );
            assert!(result.unwrap_err().to_string().contains("mls_extensions"));
        }

        // Test missing relays
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject event without relays tag");
            assert!(result.unwrap_err().to_string().contains("relays"));
        }
    }

    /// Test that relays tag validation works correctly
    #[test]
    fn test_validate_relays_tag() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test empty relays tag (should fail)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::relays(vec![]), // Empty relays tag
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject event with empty relays tag");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("at least one relay URL"),
                "Error should mention needing at least one relay URL, got: {}",
                error_msg
            );
        }

        // Test invalid relay URL format (should fail)
        {
            let invalid_tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::custom(TagKind::Relays, ["not-a-valid-url"]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(invalid_tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event with invalid relay URL format"
            );
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("Invalid relay URL"),
                "Error should mention invalid relay URL, got: {}",
                error_msg
            );
        }

        // Test multiple invalid relay URLs (should fail on first invalid one)
        {
            let invalid_tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::custom(TagKind::Relays, ["wss://valid.relay.com", "invalid-url"]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(invalid_tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event with invalid relay URL in list"
            );
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("Invalid relay URL"),
                "Error should mention invalid relay URL, got: {}",
                error_msg
            );
        }

        // Test single valid relay URL (should pass)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_ok(),
                "Should accept event with single valid relay URL, got error: {:?}",
                result
            );
        }

        // Test multiple valid relay URLs (should pass)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::relays(vec![
                    RelayUrl::parse("wss://relay1.example.com").unwrap(),
                    RelayUrl::parse("wss://relay2.example.com").unwrap(),
                    RelayUrl::parse("wss://relay3.example.com").unwrap(),
                ]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_ok(),
                "Should accept event with multiple valid relay URLs, got error: {:?}",
                result
            );
        }
    }

    /// Test that invalid protocol version values are rejected
    #[test]
    fn test_validate_invalid_protocol_version() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test invalid protocol version "2.0"
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["2.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0x000a"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject protocol version 2.0");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("Unsupported protocol version"),
                "Error should mention unsupported protocol version, got: {}",
                error_msg
            );
        }

        // Test invalid protocol version "0.9"
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["0.9"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0x000a"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject protocol version 0.9");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("Unsupported protocol version"),
                "Error should mention unsupported protocol version, got: {}",
                error_msg
            );
        }

        // Test protocol version tag without a value
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, Vec::<&str>::new()), // No value
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0x000a"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject protocol version tag without value"
            );
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("must have a value"),
                "Error should mention missing value, got: {}",
                error_msg
            );
        }
    }

    /// Test that invalid hex format in ciphersuite is rejected
    #[test]
    fn test_validate_invalid_ciphersuite_hex_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test invalid hex length (too short)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x01"]), // Too short
                Tag::custom(TagKind::MlsExtensions, ["0x0003"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject ciphersuite with invalid hex length"
            );
            assert!(result.unwrap_err().to_string().contains("6 characters"));
        }

        // Test invalid hex characters
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0xGGGG"]), // Invalid hex
                Tag::custom(TagKind::MlsExtensions, ["0x0003"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject ciphersuite with invalid hex characters"
            );
            assert!(result.unwrap_err().to_string().contains("4 hex digits"));
        }

        // Test empty ciphersuite value
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, [""]), // Empty value
                Tag::custom(TagKind::MlsExtensions, ["0x0003"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject empty ciphersuite value");
            // Empty string fails hex format validation (must be 6 characters)
            assert!(result.unwrap_err().to_string().contains("6 characters"));
        }
    }

    /// Test that invalid hex format in extensions is rejected
    #[test]
    fn test_validate_invalid_extensions_hex_format() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test invalid hex length in extensions
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x03", "0x000a"]), // First one too short
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject extension with invalid hex length"
            );
            assert!(result.unwrap_err().to_string().contains("6 characters"));
        }

        // Test invalid hex characters in extensions
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", "0xZZZZ"]), // Invalid hex
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject extension with invalid hex characters"
            );
            assert!(result.unwrap_err().to_string().contains("4 hex digits"));
        }

        // Test empty extension value
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x0003", ""]), // Empty value
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject empty extension value");
            // Empty string fails hex format validation (must be 6 characters)
            assert!(result.unwrap_err().to_string().contains("6 characters"));
        }
    }

    /// Test that invalid ciphersuite values are rejected
    #[test]
    fn test_validate_invalid_ciphersuite_values() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test unsupported ciphersuite in hex format
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0002"]), // Unsupported ciphersuite
                Tag::custom(
                    TagKind::MlsExtensions,
                    ["0x0003", "0x000a", "0x0002", "0xf2ee"],
                ),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject unsupported ciphersuite 0x0002"
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("Unsupported ciphersuite")
            );
        }

        // Test invalid ciphersuite format (non-hex string)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(
                    TagKind::MlsCiphersuite,
                    ["MLS_256_DHKEMX448_CHACHA20POLY1305_SHA512_Ed448"],
                ), // Invalid format (must be hex like 0x0001)
                Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject non-hex ciphersuite format");
            // Should fail on hex format validation, not legacy validation
            assert!(result.unwrap_err().to_string().contains("6 characters"));
        }
    }

    /// Test that missing required extensions are rejected
    #[test]
    fn test_validate_missing_required_extensions() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test missing LastResort (0x000a)
        // Note: Only the 2 required extensions from TAG_EXTENSIONS should be tested
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0xf2ee"]), // Missing 0x000a (LastResort)
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(result.is_err(), "Should reject event missing LastResort");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("0x000a"),
                "Error should contain hex code 0x000a"
            );
            assert!(
                error_msg.contains("LastResort"),
                "Error should contain extension name"
            );
        }

        // Test missing NostrGroupData (0xf2ee)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x000a"]), // Missing 0xf2ee (NostrGroupData)
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_err(),
                "Should reject event missing NostrGroupData"
            );
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("0xf2ee"),
                "Error should contain hex code 0xf2ee"
            );
            assert!(
                error_msg.contains("NostrGroupData"),
                "Error should contain extension name"
            );
        }
    }

    /// Test that uppercase hex digits are accepted (case-insensitive comparison)
    #[test]
    fn test_validate_uppercase_hex_values() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Test uppercase hex digits in extensions (case-insensitive comparison)
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x000A", "0xF2EE"]), // Uppercase hex digits
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex.clone())
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_ok(),
                "Should accept uppercase hex digits in extensions, got error: {:?}",
                result
            );
        }

        // Test mixed case hex digits in extensions
        {
            let tags = vec![
                Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
                Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
                Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xF2Ee"]), // Mixed case
                Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
                Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
            ];

            let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
                .tags(tags)
                .sign_with_keys(&nostr::Keys::generate())
                .unwrap();

            let result = mdk.validate_key_package_tags(&event, None);
            assert!(
                result.is_ok(),
                "Should accept mixed case hex digits in extensions, got error: {:?}",
                result
            );
        }
    }

    /// Test that ciphersuite tag without value is rejected
    #[test]
    fn test_validate_ciphersuite_tag_without_value() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, Vec::<&str>::new()), // No value
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(
            result.is_err(),
            "Should reject ciphersuite tag without value"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must have a value")
        );
    }

    /// Test that extensions tag without values is rejected
    #[test]
    fn test_validate_extensions_tag_without_values() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, Vec::<&str>::new()), // No values
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(TagKind::i(), [hex::encode([0xaa; 32])]),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(
            result.is_err(),
            "Should reject extensions tag without values"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at least one value")
        );
    }

    /// Test parsing a complete key package event with valid MIP-00 tags
    #[test]
    fn test_parse_key_package_with_valid_tags() {
        let mdk = create_test_mdk();
        // Use generated keys so credential identity matches event signer
        let keys = nostr::Keys::generate();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (key_package_str, tags, _hash_ref) = mdk
            .create_key_package_for_event(&keys.public_key(), relays)
            .expect("Failed to create key package");

        // Create an event signed by the same keys used in the credential
        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_str)
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        // Parse key package - should succeed
        let result = mdk.parse_key_package(&event);
        assert!(
            result.is_ok(),
            "Should parse key package with valid MIP-00 tags, got error: {:?}",
            result
        );
    }

    /// Test that parsing fails when required tags are missing
    #[test]
    fn test_parse_key_package_fails_with_missing_tags() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // Create event with missing tags
        let incomplete_tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            // Missing ciphersuite and extensions
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(incomplete_tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        // Parse key package - should fail
        let result = mdk.parse_key_package(&event);
        assert!(
            result.is_err(),
            "Should fail to parse key package with missing required tags"
        );
        assert!(result.unwrap_err().to_string().contains("Missing required"));
    }

    /// Test KeyPackage last resort extension presence and basic lifecycle (MIP-00)
    ///
    /// This test validates that KeyPackages include the last_resort extension by default
    /// and that basic KeyPackage lifecycle operations work correctly.
    ///
    /// Note: The last_resort extension signals that a KeyPackage CAN be reused by multiple
    /// group creators before the recipient processes any welcomes. However, once the recipient
    /// processes and accepts a welcome, the KeyPackage is consumed and deleted from storage.
    /// Testing true concurrent reuse would require more complex multi-device scenarios.
    ///
    /// Requirements tested:
    /// - KeyPackage includes last_resort extension
    /// - Signing key is retained after joining a group
    /// - User can successfully join a group using a KeyPackage
    /// - Key rotation proposals can be created after joining
    #[test]
    fn test_last_resort_keypackage_lifecycle() {
        // Setup: Create Bob who will be invited to a group
        let bob_keys = Keys::generate();
        let bob_mdk = create_test_mdk();
        let bob_pubkey = bob_keys.public_key();

        // Setup: Create Alice who will create the group
        let alice_keys = Keys::generate();
        let alice_mdk = create_test_mdk();

        // Step 1: Bob creates a KeyPackage with last_resort extension
        // Note: By default, MDK creates KeyPackages with last_resort extension enabled
        let relays = vec![RelayUrl::parse("wss://test.relay").unwrap()];
        let (bob_key_package_hex, tags, _hash_ref) = bob_mdk
            .create_key_package_for_event(&bob_pubkey, relays.clone())
            .expect("Failed to create key package");

        // Verify last_resort extension is present in the tags
        let extensions_tag = tags
            .iter()
            .find(|t| t.kind() == TagKind::MlsExtensions)
            .expect("Extensions tag not found");
        let extension_ids: Vec<String> = extensions_tag
            .as_slice()
            .iter()
            .skip(1) // Skip tag name
            .map(|s| s.to_string())
            .collect();
        assert!(
            extension_ids.contains(&"0x000a".to_string()),
            "KeyPackage should include last_resort extension (0x000a)"
        );

        // Create the KeyPackage event
        let bob_key_package_event = EventBuilder::new(Kind::MlsKeyPackage, bob_key_package_hex)
            .tags(tags)
            .sign_with_keys(&bob_keys)
            .expect("Failed to sign event");

        // Step 2: Alice creates a group and adds Bob using the KeyPackage
        let group_config = create_nostr_group_config_data(vec![alice_keys.public_key()]);
        let group_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package_event.clone()],
                group_config,
            )
            .expect("Failed to create group");

        // Alice merges the pending commit
        alice_mdk
            .merge_pending_commit(&group_result.group.mls_group_id)
            .expect("Failed to merge pending commit");

        // Step 3: Bob processes and accepts the welcome
        let welcome = &group_result.welcome_rumors[0];
        bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), welcome)
            .expect("Failed to process welcome");
        let pending_welcomes = bob_mdk
            .get_pending_welcomes(None)
            .expect("Failed to get pending welcomes");
        assert!(
            !pending_welcomes.is_empty(),
            "Bob should have pending welcomes after processing"
        );
        bob_mdk
            .accept_welcome(&pending_welcomes[0])
            .expect("Failed to accept welcome");

        // Verify Bob joined the group
        let bob_groups = bob_mdk.get_groups().expect("Failed to get Bob's groups");
        assert_eq!(bob_groups.len(), 1, "Bob should have joined 1 group");

        // Step 4: Verify Bob can send messages (validates signing key is retained)
        let group = &bob_groups[0];
        let rumor = crate::test_util::create_test_rumor(&bob_keys, "Test message");
        let message_result = bob_mdk.create_message(&group.mls_group_id, rumor);
        assert!(
            message_result.is_ok(),
            "Bob should be able to send messages (signing key retained)"
        );

        // Step 5: Verify key rotation can be performed
        let rotation_result = bob_mdk.self_update(&group.mls_group_id);
        assert!(rotation_result.is_ok(), "Bob should be able to rotate keys");

        // Verify the rotation created a proposal
        let rotation_result_data = rotation_result.expect("Rotation should succeed");
        assert_eq!(
            rotation_result_data.evolution_event.kind,
            Kind::MlsGroupMessage,
            "Rotation should create a group message event"
        );

        // Note: Testing true concurrent KeyPackage reuse (multiple group creators using the same
        // KeyPackage before the recipient processes any welcomes) would require a more complex
        // test setup with careful timing control. The last_resort extension enables this at the
        // protocol level, but the current test validates the extension is present and basic
        // lifecycle works correctly.
    }

    #[test]
    fn test_key_package_base64_encoding() {
        let config = crate::MdkConfig::default();

        let mdk = crate::tests::create_test_mdk_with_config(config);
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (key_package_str, tags, _hash_ref) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create key package");

        assert!(
            BASE64.decode(&key_package_str).is_ok(),
            "Content should be valid base64, got: {}",
            key_package_str
        );

        let encoding_tag = tags
            .iter()
            .find(|t| t.as_slice().first() == Some(&"encoding".to_string()));
        assert!(encoding_tag.is_some(), "Should have encoding tag");
        assert_eq!(
            encoding_tag.unwrap().as_slice().get(1).map(|s| s.as_str()),
            Some("base64"),
            "Encoding tag should be 'base64'"
        );

        let parsed = mdk
            .parse_serialized_key_package(&key_package_str, ContentEncoding::Base64)
            .expect("Failed to parse base64 key package");
        assert_eq!(parsed.ciphersuite(), DEFAULT_CIPHERSUITE);
    }

    /// Test that key packages are always created with base64
    #[test]
    fn test_key_package_parsing_base64() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (base64_key_package, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, relays)
            .expect("Failed to create base64 key package");

        assert!(
            BASE64.decode(&base64_key_package).is_ok(),
            "Created key package should be base64"
        );

        assert!(
            mdk.parse_serialized_key_package(&base64_key_package, ContentEncoding::Base64)
                .is_ok(),
            "Should parse base64 key package"
        );
    }

    /// Security test: Identity binding prevents impersonation attacks
    ///
    /// This test verifies that parse_key_package rejects key packages where the
    /// BasicCredential.identity (Nostr public key) doesn't match the event signer.
    ///
    /// Attack scenario being tested:
    /// 1. Attacker creates a KeyPackage with victim's Nostr public key in the credential
    /// 2. Attacker signs the kind-443 event with their own key
    /// 3. If a group admin processes this, the attacker could join appearing as the victim
    ///
    /// This test ensures such attacks are prevented by the identity binding check.
    #[test]
    fn test_parse_key_package_rejects_identity_mismatch() {
        let mdk = create_test_mdk();
        let victim_keys = nostr::Keys::generate();
        let attacker_keys = nostr::Keys::generate();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        // Create a key package with the victim's public key in the credential
        let (key_package_hex, tags, _hash_ref) = mdk
            .create_key_package_for_event(&victim_keys.public_key(), relays)
            .expect("Failed to create key package");

        // Attacker signs the event with their own keys (NOT the victim's keys)
        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&attacker_keys)
            .unwrap();

        // Parse should fail because credential identity (victim) != event signer (attacker)
        let result = mdk.parse_key_package(&event);
        assert!(
            result.is_err(),
            "Should reject key package with identity mismatch"
        );

        // Verify we get the correct error type
        let error = result.unwrap_err();
        match error {
            Error::KeyPackageIdentityMismatch {
                credential_identity,
                event_signer,
            } => {
                assert_eq!(
                    credential_identity,
                    victim_keys.public_key().to_hex(),
                    "credential_identity should be victim's public key"
                );
                assert_eq!(
                    event_signer,
                    attacker_keys.public_key().to_hex(),
                    "event_signer should be attacker's public key"
                );
            }
            _ => panic!(
                "Expected KeyPackageIdentityMismatch error, got: {:?}",
                error
            ),
        }
    }

    /// Security test: Valid identity binding is accepted
    ///
    /// This test verifies that parse_key_package accepts key packages where the
    /// BasicCredential.identity matches the event signer (the legitimate case).
    #[test]
    fn test_parse_key_package_accepts_matching_identity() {
        let mdk = create_test_mdk();
        let keys = nostr::Keys::generate();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        // Create a key package with the user's public key
        let (key_package_hex, tags, _hash_ref) = mdk
            .create_key_package_for_event(&keys.public_key(), relays)
            .expect("Failed to create key package");

        // Sign the event with the same keys (legitimate scenario)
        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        // Parse should succeed because credential identity == event signer
        let result = mdk.parse_key_package(&event);
        assert!(
            result.is_ok(),
            "Should accept key package with matching identity, got error: {:?}",
            result
        );

        // Verify the parsed key package has the correct identity
        let key_package = result.unwrap();
        let credential =
            BasicCredential::try_from(key_package.leaf_node().credential().clone()).unwrap();
        let parsed_pubkey = mdk
            .parse_credential_identity(credential.identity())
            .unwrap();
        assert_eq!(
            parsed_pubkey,
            keys.public_key(),
            "Parsed key package should have the correct identity"
        );
    }

    /// Test that the `i` tag contains a valid hex-encoded KeyPackageRef that matches
    /// the independently computed value from the key package content.
    #[test]
    fn test_i_tag_contains_valid_key_package_ref() {
        let mdk = create_test_mdk();
        let keys = nostr::Keys::generate();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (key_package_str, tags, _hash_ref) = mdk
            .create_key_package_for_event(&keys.public_key(), relays)
            .expect("Failed to create key package");

        // Find the i tag
        let i_tag = tags
            .iter()
            .find(|t| t.kind() == TagKind::i())
            .expect("i tag not found");
        let i_tag_value = i_tag.content().expect("i tag should have content");

        // Verify it's valid hex
        let i_tag_bytes = hex::decode(i_tag_value).expect("i tag value should be valid hex");
        assert!(
            !i_tag_bytes.is_empty(),
            "i tag should contain non-empty hex data"
        );

        // Parse the key package and compute the KeyPackageRef independently
        let key_package = mdk
            .parse_serialized_key_package(&key_package_str, ContentEncoding::Base64)
            .expect("Failed to parse key package");
        let computed_ref = key_package
            .hash_ref(mdk.provider.crypto())
            .expect("Failed to compute hash_ref");
        let computed_ref_hex = hex::encode(computed_ref.as_slice());

        assert_eq!(
            i_tag_value, computed_ref_hex,
            "i tag value should match the computed KeyPackageRef"
        );
    }

    /// Test that parse_key_package rejects events where the `i` tag value
    /// does not match the computed KeyPackageRef from the content.
    #[test]
    fn test_parse_key_package_rejects_mismatched_i_tag() {
        let mdk = create_test_mdk();
        let keys = nostr::Keys::generate();
        let relays = vec![RelayUrl::parse("wss://relay.example.com").unwrap()];

        let (key_package_str, mut tags, _hash_ref) = mdk
            .create_key_package_for_event(&keys.public_key(), relays)
            .expect("Failed to create key package");

        // Replace the i tag with a fabricated (wrong) value
        let i_tag_idx = tags
            .iter()
            .position(|t| t.kind() == TagKind::i())
            .expect("i tag should exist");
        tags[i_tag_idx] = Tag::custom(TagKind::i(), [hex::encode([0xff; 32])]);

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_str)
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let result = mdk.parse_key_package(&event);
        assert!(
            result.is_err(),
            "Should reject key package with mismatched i tag"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("does not match"),
            "Error should mention mismatch, got: {}",
            error_msg
        );
    }

    /// Test that validate_key_package_tags rejects events missing the `i` tag
    #[test]
    fn test_validate_missing_i_tag() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        // All required tags present EXCEPT i
        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(result.is_err(), "Should reject event without i tag");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Missing required tag: i"),
            "Error should mention missing i tag, got: {}",
            error_msg
        );
    }

    /// Test that validate_key_package_tags rejects events with invalid hex in the `i` tag
    #[test]
    fn test_validate_invalid_i_tag_hex() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(TagKind::i(), ["not-valid-hex!@#"]),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(
            result.is_err(),
            "Should reject event with invalid hex in i tag"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("valid hex"),
            "Error should mention invalid hex, got: {}",
            error_msg
        );
    }

    /// Test that validate_key_package_tags rejects events with empty `i` tag (no value)
    #[test]
    fn test_validate_empty_i_tag() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(TagKind::i(), Vec::<&str>::new()),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(result.is_err(), "Should reject event with empty i tag");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("exactly one value"),
            "Error should mention exactly one value, got: {}",
            error_msg
        );
    }

    /// Test that validate_key_package_tags rejects events with multi-value `i` tag
    #[test]
    fn test_validate_multi_value_i_tag() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(
                TagKind::i(),
                [hex::encode([0xaa; 32]), hex::encode([0xbb; 32])],
            ),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(
            result.is_err(),
            "Should reject event with multi-value i tag"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("exactly one value"),
            "Error should mention exactly one value, got: {}",
            error_msg
        );
    }

    /// Test that validate_key_package_tags rejects events with empty string in `i` tag
    #[test]
    fn test_validate_empty_string_i_tag() {
        let mdk = create_test_mdk();
        let test_pubkey =
            PublicKey::from_hex("884704bd421671e01c13f854d2ce23ce2a5bfe9562f4f297ad2bc921ba30c3a6")
                .unwrap();

        let (key_package_hex, _, _) = mdk
            .create_key_package_for_event(&test_pubkey, vec![])
            .expect("Failed to create key package");

        let tags = vec![
            Tag::custom(TagKind::MlsProtocolVersion, ["1.0"]),
            Tag::custom(TagKind::MlsCiphersuite, ["0x0001"]),
            Tag::custom(TagKind::MlsExtensions, ["0x000a", "0xf2ee"]),
            Tag::relays(vec![RelayUrl::parse("wss://relay.example.com").unwrap()]),
            Tag::custom(TagKind::i(), [""]),
        ];

        let event = EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
            .tags(tags)
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();

        let result = mdk.validate_key_package_tags(&event, None);
        assert!(
            result.is_err(),
            "Should reject event with empty string i tag value"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("must not be empty"),
            "Error should mention empty value, got: {}",
            error_msg
        );
    }
}
