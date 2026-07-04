//! Node id minting: deterministic UUID derivation and the short-form
//! display helper.

use super::*;
use crate::machines::scheduler::graph::derive_node_id;

// ── derive_node_id ───────────────────────────────────────────────────────

#[test]
fn same_seed_and_counter_always_derive_the_same_id() {
    // Invariant: minting is a pure function of (seed, counter), so
    // SchedulerMachine::transition stays deterministic given (state, event)
    // even though ids are UUID-formatted.
    let a = derive_node_id(42, 7);
    let b = derive_node_id(42, 7);
    assert_eq!(a, b);
}

#[test]
fn different_counters_derive_different_ids_for_the_same_seed() {
    let ids: Vec<_> = (0..20).map(|i| derive_node_id(42, i)).collect();
    let mut unique = ids.clone();
    unique.sort_by(|a, b| a.0.cmp(&b.0));
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "20 sequential counters must never collide"
    );
}

#[test]
fn different_seeds_derive_different_ids_for_the_same_counter() {
    assert_ne!(derive_node_id(1, 0), derive_node_id(2, 0));
}

#[test]
fn derived_id_is_formatted_as_a_standard_v4_uuid() {
    // Invariant: even though the bits are deterministically derived rather
    // than drawn from an RNG, the string must read as a normal v4 UUID
    // (correct length, hyphen positions, version and variant nibbles) so
    // tooling and users can't tell the difference.
    let id = derive_node_id(0x1234_5678_9abc_def0_1122_3344_5566_7788, 3);
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
    let id = derive_node_id(1, 1);
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
