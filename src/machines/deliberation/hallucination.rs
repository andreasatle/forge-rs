//! Detection of provably false structural-defect claims in Critic/Referee
//! rejection reasons.
//!
//! By the time a Critic or Referee rejection reaches [`super::machine`],
//! the Producer content it is judging has already passed grammar-constrained
//! decoding and structural validation (see `ValidateProducer` in
//! [`super::effect::DeliberationEffect`]). A rejection reason that asserts
//! the content has a syntax error, is malformed/invalid JSON, or has an
//! unclosed field is therefore not a judgment call — it is a checkable claim
//! about content whose structural validity is already a machine invariant,
//! and the claim is false whenever it fires. This module only classifies
//! that narrow, binary class of claim; it makes no attempt to fact-check
//! anything else a Critic or Referee might say.

/// Substrings (checked case-insensitively) that indicate a rejection reason
/// is asserting a structural JSON defect in already-validated content.
const STRUCTURAL_DEFECT_PATTERNS: &[&str] = &[
    "syntax error",
    "syntax errors",
    "invalid json",
    "malformed json",
    "malformed structure",
    "not valid json",
    "not well-formed json",
    "unclosed field",
    "unclosed bracket",
    "unclosed brace",
    "unclosed string",
    "unclosed quote",
    "unterminated string",
    "missing closing brace",
    "missing closing bracket",
    "missing closing quote",
    "trailing comma",
    "unexpected token",
    "json parsing error",
    "failed to parse json",
    "parse error",
    "does not parse as json",
    "is not parseable",
];

/// Returns `true` when `reason` asserts a structural JSON defect — a claim
/// that is mechanically impossible for content that already passed
/// grammar-constrained decoding and structural validation before Critic/
/// Referee ever saw it.
pub(super) fn is_structural_defect_claim(reason: &str) -> bool {
    let lowered = reason.to_lowercase();
    STRUCTURAL_DEFECT_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_structural_defect_phrasings() {
        // Invariant: every phrasing this module claims to catch must actually
        // be caught, case-insensitively, regardless of surrounding text.
        for reason in [
            "The output has a syntax error in the task_kv field.",
            "This is INVALID JSON and cannot be accepted.",
            "The JSON is malformed json near the tasks array.",
            "There is an unclosed field in the objective.",
            "Rejected: unexpected token at position 42.",
            "Parse error: trailing comma before closing brace.",
        ] {
            assert!(
                is_structural_defect_claim(reason),
                "expected {reason:?} to be classified as a structural defect claim"
            );
        }
    }

    #[test]
    fn does_not_flag_legitimate_semantic_rejections() {
        // Invariant: genuine judgment-based rejections (clarity, overlap,
        // incompleteness) must never be misclassified as hallucinated.
        for reason in [
            "The task lacks a clear definition of what the function should do.",
            "This decomposition is not collectively exhaustive.",
            "Two sibling tasks overlap on the same file.",
            "The objective should specify handling for edge cases.",
        ] {
            assert!(
                !is_structural_defect_claim(reason),
                "did not expect {reason:?} to be classified as a structural defect claim"
            );
        }
    }
}
