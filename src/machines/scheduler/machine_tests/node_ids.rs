//! Node id minting: random UUID generation and the short-form display helper.

use super::*;
use crate::machines::scheduler::graph::new_node_id;

// ── new_node_id ────────────────────────────────────────────────────────────

#[test]
fn successive_calls_never_collide() {
    let ids: Vec<_> = (0..20).map(|_| new_node_id()).collect();
    let mut unique = ids.clone();
    unique.sort_by(|a, b| a.0.cmp(&b.0));
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "20 freshly minted ids must never collide"
    );
}

#[test]
fn minted_id_is_formatted_as_a_standard_v4_uuid() {
    // Invariant: the string must read as a normal v4 UUID (correct length,
    // hyphen positions, version and variant nibbles) so tooling and users
    // can rely on the format.
    let id = new_node_id();
    let s = &id.0;
    assert_eq!(s.len(), 36, "UUID string must be 36 characters: {s}");
    assert_eq!(s.as_bytes()[8], b'-');
    assert_eq!(s.as_bytes()[13], b'-');
    assert_eq!(s.as_bytes()[18], b'-');
    assert_eq!(s.as_bytes()[23], b'-');
    assert_eq!(&s[14..15], "4", "version nibble must be 4: {s}");
    assert!(
        matches!(s.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
        "variant nibble must be RFC 4122: {s}"
    );
}

// ── NodeId::short ─────────────────────────────────────────────────────────

#[test]
fn short_returns_the_first_8_characters_of_a_long_id() {
    let id = new_node_id();
    assert_eq!(id.short().len(), 8);
    assert_eq!(id.short(), &id.0[..8]);
}

#[test]
fn short_returns_the_whole_string_when_shorter_than_8_characters() {
    // Test-fixture ids (e.g. "A", "root") are shorter than a UUID prefix;
    // short() must not panic or truncate them further.
    assert_eq!(NodeId("A".to_string()).short(), "A");
    assert_eq!(NodeId("".to_string()).short(), "");
}
