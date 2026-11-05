//! PROVISIONAL owner decisions (spec "Provisional owner decisions").
//!
//! The owner has not decided these. The most conservative choice is built
//! in; each is one constant here so changing it is a one-line change.

/// P-A: a locked phone shows no request content. The push carries this
/// fixed generic title only, never a machine name or a permission title.
pub const WAKE_TITLE: &str = "Agent inbox";
/// P-A: fixed generic body.
pub const WAKE_BODY: &str = "An agent may need you";

/// P-B: no relay-side revocation of one handle. The accepted abuse window is
/// this lifetime (30 days) plus the device-side limits.
pub const HANDLE_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;

/// P-C: a stolen push address is a documented limit. A handle is sealed and
/// bound to platform, app and address; nothing binds it to a device beyond
/// that. (No constant: the behaviour is the absence of an attestation step.)
pub const _P_C_DOCUMENTED_LIMIT: () = ();
