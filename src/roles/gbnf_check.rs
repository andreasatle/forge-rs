//! A minimal GBNF (llama.cpp grammar) recognizer, used only by tests to
//! prove a grammar constant genuinely accepts or rejects a candidate string
//! — rather than merely asserting that two Rust-side constant strings
//! differ, which proves nothing about what the grammar actually permits.
//!
//! Supports exactly the GBNF subset used by this crate's grammar constants:
//! rule references, string literals, character classes (negation, ranges,
//! and `\n`/`\t`/`\r`/`\xHH` escapes), grouping, alternation (`|`), and the
//! `?`/`*`/`+` quantifiers.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
enum Elem {
    Literal(Vec<u8>),
    CharClass {
        negated: bool,
        ranges: Vec<(u8, u8)>,
    },
    Rule(String),
    Group(Vec<Vec<Elem>>),
    Opt(Box<Elem>),
    Star(Box<Elem>),
    Plus(Box<Elem>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Assign,
    Pipe,
    LParen,
    RParen,
    Question,
    Star,
    Plus,
    Str(Vec<u8>),
    Class {
        negated: bool,
        ranges: Vec<(u8, u8)>,
    },
    End,
}

fn unescape_one(chars: &[char], i: &mut usize) -> u8 {
    let e = chars[*i];
    *i += 1;
    match e {
        'n' => b'\n',
        't' => b'\t',
        'r' => b'\r',
        '\\' => b'\\',
        '"' => b'"',
        ']' => b']',
        '^' => b'^',
        '-' => b'-',
        'x' => {
            let hex: String = chars[*i..*i + 2].iter().collect();
            *i += 2;
            u8::from_str_radix(&hex, 16).expect("valid \\xHH escape")
        }
        other => other as u8,
    }
}

fn tokenize(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ':' && chars.get(i + 1) == Some(&':') && chars.get(i + 2) == Some(&'=') {
            out.push(Tok::Assign);
            i += 3;
            continue;
        }
        match c {
            '|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '?' => {
                out.push(Tok::Question);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '"' => {
                i += 1;
                let mut bytes = Vec::new();
                while chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                        bytes.push(unescape_one(&chars, &mut i));
                    } else {
                        bytes.push(chars[i] as u8);
                        i += 1;
                    }
                }
                i += 1;
                out.push(Tok::Str(bytes));
            }
            '[' => {
                i += 1;
                let negated = if chars.get(i) == Some(&'^') {
                    i += 1;
                    true
                } else {
                    false
                };
                let mut ranges = Vec::new();
                while chars[i] != ']' {
                    let lo = if chars[i] == '\\' {
                        i += 1;
                        unescape_one(&chars, &mut i)
                    } else {
                        let b = chars[i] as u8;
                        i += 1;
                        b
                    };
                    if chars.get(i) == Some(&'-') && chars.get(i + 1) != Some(&']') {
                        i += 1;
                        let hi = if chars[i] == '\\' {
                            i += 1;
                            unescape_one(&chars, &mut i)
                        } else {
                            let b = chars[i] as u8;
                            i += 1;
                            b
                        };
                        ranges.push((lo, hi));
                    } else {
                        ranges.push((lo, lo));
                    }
                }
                i += 1;
                out.push(Tok::Class { negated, ranges });
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-')
                {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            other => panic!("gbnf tokenizer: unexpected char {other:?} at byte offset {i}"),
        }
    }
    out.push(Tok::End);
    out
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        self.pos += 1;
        t
    }

    fn parse_grammar(&mut self) -> HashMap<String, Vec<Vec<Elem>>> {
        let mut rules = HashMap::new();
        while *self.peek() != Tok::End {
            let name = match self.next() {
                Tok::Ident(n) => n,
                other => panic!("expected rule name, got {other:?}"),
            };
            assert_eq!(
                self.next(),
                Tok::Assign,
                "expected '::=' after rule name {name}"
            );
            let alts = self.parse_alt();
            rules.insert(name, alts);
        }
        rules
    }

    fn parse_alt(&mut self) -> Vec<Vec<Elem>> {
        let mut alts = vec![self.parse_seq()];
        while *self.peek() == Tok::Pipe {
            self.next();
            alts.push(self.parse_seq());
        }
        alts
    }

    fn parse_seq(&mut self) -> Vec<Elem> {
        let mut seq = Vec::new();
        loop {
            match self.peek() {
                Tok::Str(_) | Tok::Class { .. } | Tok::LParen => {
                    seq.push(self.parse_term());
                }
                Tok::Ident(_) => {
                    // An identifier immediately followed by `::=` starts the
                    // next rule definition, not a rule reference in this
                    // sequence.
                    if self.toks.get(self.pos + 1) == Some(&Tok::Assign) {
                        break;
                    }
                    seq.push(self.parse_term());
                }
                _ => break,
            }
        }
        seq
    }

    fn parse_term(&mut self) -> Elem {
        let atom = self.parse_atom();
        match self.peek() {
            Tok::Question => {
                self.next();
                Elem::Opt(Box::new(atom))
            }
            Tok::Star => {
                self.next();
                Elem::Star(Box::new(atom))
            }
            Tok::Plus => {
                self.next();
                Elem::Plus(Box::new(atom))
            }
            _ => atom,
        }
    }

    fn parse_atom(&mut self) -> Elem {
        match self.next() {
            Tok::Str(bytes) => Elem::Literal(bytes),
            Tok::Class { negated, ranges } => Elem::CharClass { negated, ranges },
            Tok::Ident(name) => Elem::Rule(name),
            Tok::LParen => {
                let alts = self.parse_alt();
                assert_eq!(self.next(), Tok::RParen, "expected ')'");
                Elem::Group(alts)
            }
            other => panic!("unexpected token in atom position: {other:?}"),
        }
    }
}

/// A parsed GBNF grammar that can check whether it accepts a candidate
/// string in full (start to end, via its `root` rule).
pub(crate) struct Grammar {
    rules: HashMap<String, Vec<Vec<Elem>>>,
}

impl Grammar {
    /// Parse GBNF source text, as written in this crate's grammar constants.
    ///
    /// Panics on malformed input — acceptable here since this only ever
    /// parses grammar constants this crate owns and controls, not untrusted
    /// input.
    pub(crate) fn parse(src: &str) -> Self {
        let toks = tokenize(src);
        let mut parser = Parser { toks, pos: 0 };
        let rules = parser.parse_grammar();
        assert!(
            rules.contains_key("root"),
            "grammar must define a 'root' rule"
        );
        Self { rules }
    }

    /// Whether this grammar accepts `input` in its entirety, starting from
    /// `root`.
    pub(crate) fn accepts(&self, input: &str) -> bool {
        let bytes = input.as_bytes();
        self.match_rule("root", bytes, 0).contains(&bytes.len())
    }

    /// All positions reachable after fully matching rule `name` starting at
    /// `pos`. Returned as a set (rather than a single length) because
    /// alternation and quantifiers make multiple consumption lengths valid.
    fn match_rule(&self, name: &str, input: &[u8], pos: usize) -> HashSet<usize> {
        let alts = self
            .rules
            .get(name)
            .unwrap_or_else(|| panic!("undefined rule: {name}"));
        self.match_alts(alts, input, pos)
    }

    fn match_alts(&self, alts: &[Vec<Elem>], input: &[u8], pos: usize) -> HashSet<usize> {
        let mut all = HashSet::new();
        for seq in alts {
            all.extend(self.match_seq(seq, input, pos));
        }
        all
    }

    fn match_seq(&self, seq: &[Elem], input: &[u8], pos: usize) -> HashSet<usize> {
        let mut positions: HashSet<usize> = HashSet::from([pos]);
        for elem in seq {
            let mut next = HashSet::new();
            for &p in &positions {
                next.extend(self.match_elem(elem, input, p));
            }
            positions = next;
            if positions.is_empty() {
                break;
            }
        }
        positions
    }

    fn match_elem(&self, elem: &Elem, input: &[u8], pos: usize) -> HashSet<usize> {
        match elem {
            Elem::Literal(bytes) => {
                if input[pos..].starts_with(bytes.as_slice()) {
                    HashSet::from([pos + bytes.len()])
                } else {
                    HashSet::new()
                }
            }
            Elem::CharClass { negated, ranges } => {
                if pos < input.len() {
                    let b = input[pos];
                    let in_class = ranges.iter().any(|&(lo, hi)| b >= lo && b <= hi);
                    if in_class != *negated {
                        HashSet::from([pos + 1])
                    } else {
                        HashSet::new()
                    }
                } else {
                    HashSet::new()
                }
            }
            Elem::Rule(name) => self.match_rule(name, input, pos),
            Elem::Group(alts) => self.match_alts(alts, input, pos),
            Elem::Opt(inner) => {
                let mut s = self.match_elem(inner, input, pos);
                s.insert(pos);
                s
            }
            // Fixpoint over reachable positions rather than naive recursion:
            // terminates even when `inner` can match zero characters,
            // because `all` only ever grows and is bounded by input length.
            Elem::Star(inner) => {
                let mut all: HashSet<usize> = HashSet::from([pos]);
                let mut frontier = all.clone();
                loop {
                    let mut next = HashSet::new();
                    for &p in &frontier {
                        for np in self.match_elem(inner, input, p) {
                            if all.insert(np) {
                                next.insert(np);
                            }
                        }
                    }
                    if next.is_empty() {
                        break;
                    }
                    frontier = next;
                }
                all
            }
            Elem::Plus(inner) => {
                let mut all = HashSet::new();
                for p in self.match_elem(inner, input, pos) {
                    all.extend(self.match_elem(&Elem::Star(inner.clone()), input, p));
                }
                all
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Grammar;

    #[test]
    fn accepts_matching_producer_summary_shape() {
        let grammar = Grammar::parse(crate::roles::policy::PRODUCER_GBNF);
        assert!(grammar.accepts(r#"{"summary":"did the thing"}"#));
    }

    #[test]
    fn rejects_wrong_field_name() {
        let grammar = Grammar::parse(crate::roles::policy::PRODUCER_GBNF);
        assert!(!grammar.accepts(r#"{"result":"did the thing"}"#));
    }

    #[test]
    fn rejects_trailing_garbage() {
        let grammar = Grammar::parse(crate::roles::policy::PRODUCER_GBNF);
        assert!(!grammar.accepts(r#"{"summary":"did the thing"} extra"#));
    }

    #[test]
    fn accepts_either_branch_of_a_top_level_alternation() {
        let grammar = Grammar::parse(crate::roles::policy::ROLE_GBNF);
        assert!(grammar.accepts(r#"{"status":"accepted","content":"ok"}"#));
        assert!(grammar.accepts(r#"{"status":"rejected","reason":"no"}"#));
        assert!(!grammar.accepts(r#"{"status":"maybe","content":"ok"}"#));
    }

    // Regression test for a runaway-generation bug: the root rule used to end
    // in an unbounded `ws` after the closing brace, so a grammar-constrained
    // model could keep sampling whitespace forever after a complete answer,
    // running every call to the n_predict ceiling. The root rule must end at
    // the closing brace/bracket so no further tokens are grammar-legal.
    #[test]
    fn rejects_trailing_whitespace_after_closing_brace() {
        let producer = Grammar::parse(crate::roles::policy::PRODUCER_GBNF);
        assert!(!producer.accepts("{\"summary\":\"did the thing\"} \n"));

        let role = Grammar::parse(crate::roles::policy::ROLE_GBNF);
        assert!(!role.accepts("{\"status\":\"accepted\",\"content\":\"ok\"}\n\n"));

        let producer_tool = Grammar::parse(crate::roles::policy::PRODUCER_TOOL_GBNF);
        assert!(!producer_tool.accepts("{\"tool\":\"list_files\"} "));

        let reviewer_tool = Grammar::parse(crate::roles::policy::REVIEWER_TOOL_GBNF);
        assert!(!reviewer_tool.accepts("{\"tool\":\"list_files\"} "));

        let planner_with_roles = Grammar::parse(crate::roles::policy::PLANNER_GBNF_WITH_ROLES);
        assert!(planner_with_roles.accepts(
            "{\"kind\":\"task\",\"tasks\":[{\"id\":\"a\",\"objective\":\"o\",\"task_kv\":{\"name\":\"n\"},\"depends_on\":[]}]}"
        ));
        assert!(!planner_with_roles.accepts(
            "{\"kind\":\"task\",\"tasks\":[{\"id\":\"a\",\"objective\":\"o\",\"task_kv\":{\"name\":\"n\"},\"depends_on\":[]}]} "
        ));

        let planner_no_work = Grammar::parse(crate::roles::policy::PLANNER_GBNF_NO_WORK);
        assert!(planner_no_work.accepts(
            "{\"kind\":\"task\",\"tasks\":[{\"id\":\"a\",\"objective\":\"o\",\"task_kv\":{\"name\":\"n\"},\"depends_on\":[]}]}"
        ));
        assert!(!planner_no_work.accepts(
            "{\"kind\":\"task\",\"tasks\":[{\"id\":\"a\",\"objective\":\"o\",\"task_kv\":{\"name\":\"n\"},\"depends_on\":[]}]} "
        ));
    }

    // ── task_kv open string-keyed map ───────────────────────────────────────

    #[test]
    fn task_kv_accepts_arbitrary_key_sets() {
        // Invariant: the grammar constrains task_kv's *shape* (a JSON object
        // of string keys to string values) but not which keys or how many —
        // that is validated post-parse from adapter YAML, not baked into the
        // grammar. Prove a zero-key, one-key, and multi-key object are all
        // grammar-legal for the same rule.
        let planner_no_work = Grammar::parse(crate::roles::policy::PLANNER_GBNF_NO_WORK);
        let with_kv = |kv: &str| {
            format!(
                "{{\"kind\":\"task\",\"tasks\":[{{\"id\":\"a\",\"objective\":\"o\",\"task_kv\":{kv},\"depends_on\":[]}}]}}"
            )
        };
        assert!(planner_no_work.accepts(&with_kv("{}")));
        assert!(planner_no_work.accepts(&with_kv("{\"file_path\":\"main.py\"}")));
        assert!(planner_no_work.accepts(&with_kv(
            "{\"name\":\"fibonacci\",\"function_name\":\"fibonacci\",\"file_path\":\"main.py\"}"
        )));
        // Malformed task_kv (non-string value, missing colon) must be rejected.
        assert!(!planner_no_work.accepts(&with_kv("{\"file_path\":1}")));
        assert!(!planner_no_work.accepts(&with_kv("{\"file_path\"}")));
    }
}
