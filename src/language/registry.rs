//! Language spec registry — look up a bundled [`LanguageSpec`] by name.

use super::spec::LanguageSpec;

const RUST_SPEC: &str = include_str!("rust.yaml");

/// Return the [`LanguageSpec`] for `id`, or `None` if the language is unknown.
///
/// Bundled specs are parsed from YAML at call time. Panics if a bundled spec
/// fails to parse — that is a compile-time authoring error, not a runtime one.
pub fn language_spec(id: &str) -> Option<LanguageSpec> {
    match id {
        "rust" => {
            Some(serde_yaml::from_str(RUST_SPEC).expect("bundled rust.yaml must be valid YAML"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_spec_loads_correctly() {
        let spec = language_spec("rust").expect("rust spec must load");
        assert!(
            !spec.prompt_guidance.is_empty(),
            "rust spec must have prompt guidance"
        );
        assert!(
            !spec.init.commands.is_empty(),
            "rust spec must have at least one init command"
        );
        assert!(
            !spec.validation.commands.is_empty(),
            "rust spec must have at least one validation command"
        );
    }

    #[test]
    fn rust_init_contains_cargo_init_vcs_none() {
        let spec = language_spec("rust").expect("rust spec must load");
        let cmd = &spec.init.commands[0];
        assert_eq!(cmd.program, "cargo", "init program must be cargo");
        assert!(
            cmd.args.iter().any(|a| a == "init"),
            "init args must include 'init'; got: {:?}",
            cmd.args
        );
        assert!(
            cmd.args
                .windows(2)
                .any(|w| w[0] == "--vcs" && w[1] == "none"),
            "init must pass --vcs none; got: {:?}",
            cmd.args
        );
        assert!(
            cmd.args.last() == Some(&".".to_string()),
            "init must target the current directory; got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn rust_validation_contains_fmt_check_check_test() {
        let spec = language_spec("rust").expect("rust spec must load");
        let cmds = &spec.validation.commands;

        assert!(
            cmds.iter().all(|c| c.program == "cargo"),
            "all validation commands must use cargo; got: {cmds:?}"
        );

        let has_fmt_check = cmds.iter().any(|c| {
            c.args.contains(&"fmt".to_string()) && c.args.contains(&"--check".to_string())
        });
        assert!(
            has_fmt_check,
            "validation must include cargo fmt --check; got: {cmds:?}"
        );

        let has_check = cmds.iter().any(|c| c.args == vec!["check"]);
        assert!(
            has_check,
            "validation must include cargo check; got: {cmds:?}"
        );

        let has_test = cmds.iter().any(|c| c.args == vec!["test"]);
        assert!(
            has_test,
            "validation must include cargo test; got: {cmds:?}"
        );
    }

    #[test]
    fn unknown_language_returns_none() {
        assert!(language_spec("java").is_none(), "java must be unknown");
        assert!(language_spec("python").is_none(), "python must be unknown");
        assert!(language_spec("").is_none(), "empty string must be unknown");
    }
}
