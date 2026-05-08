//! MDK constants

use openmls::extensions::ExtensionType;
use openmls_traits::types::Ciphersuite;

/// Nostr Group Data extension type
pub const NOSTR_GROUP_DATA_EXTENSION_TYPE: u16 = 0xF2EE; // Be FREE

/// Default ciphersuite for Nostr Groups.
/// This is also the only required ciphersuite for Nostr Groups.
pub const DEFAULT_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Extensions that clients advertise support for in their KeyPackage capabilities.
///
/// Per RFC 9420 Section 7.2, this should only include non-default extensions that
/// the client supports. Default extensions (RequiredCapabilities, RatchetTree,
/// ApplicationId, ExternalPub, ExternalSenders) are assumed to be supported by all
/// clients and should NOT be listed here.
///
/// Note: LastResort (0x000a) is included here because OpenMLS requires KeyPackage-level
/// extensions to be declared in capabilities for validation, even though per the MLS
/// Extensions draft it's technically just a KeyPackage marker.
pub const SUPPORTED_EXTENSIONS: [ExtensionType; 2] = [
    ExtensionType::LastResort, // 0x000A - Required by OpenMLS validation
    ExtensionType::Unknown(NOSTR_GROUP_DATA_EXTENSION_TYPE), // 0xF2EE - NostrGroupData
];

/// Extensions that are required in the GroupContext RequiredCapabilities extension.
///
/// This enforces that all group members must support these extensions. For Marmot,
/// we require the NostrGroupData extension (0xF2EE) to ensure all members can
/// process the Nostr-specific group metadata.
pub const GROUP_CONTEXT_REQUIRED_EXTENSIONS: [ExtensionType; 1] = [
    ExtensionType::Unknown(NOSTR_GROUP_DATA_EXTENSION_TYPE), // 0xF2EE - NostrGroupData
];

/// Extensions that are advertised in Nostr event tags (mls_extensions tag).
///
/// This MUST match SUPPORTED_EXTENSIONS to accurately advertise what the
/// KeyPackage capabilities contain. This allows other clients to validate
/// compatibility before attempting to add this user to a group.
///
/// Note: GREASE values are NOT included here. GREASE is injected dynamically
/// into Capabilities at runtime (see `MDK::capabilities()`) but should not be
/// advertised in tags since they are meant for extensibility testing only and
/// will vary between clients/invocations.
pub const TAG_EXTENSIONS: [ExtensionType; 2] = [
    ExtensionType::LastResort, // 0x000A - Required in capabilities
    ExtensionType::Unknown(NOSTR_GROUP_DATA_EXTENSION_TYPE), // 0xF2EE - NostrGroupData
];
