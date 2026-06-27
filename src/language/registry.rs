//! Language spec registry — look up a bundled [`LanguageSpec`] by name.

use super::spec::LanguageSpec;

const RUST_SPEC: &str = include_str!("rust.yaml");
const PYTHON_SPEC: &str = include_str!("python.yaml");

/// Return the [`LanguageSpec`] for `id`, or `None` if the language is unknown.
///
/// Bundled specs are parsed from YAML at call time. Panics if a bundled spec
/// fails to parse — that is a compile-time authoring error, not a runtime one.
pub fn language_spec(id: &str) -> Option<LanguageSpec> {
    match id {
        "rust" => {
            Some(serde_yaml::from_str(RUST_SPEC).expect("bundled rust.yaml must be valid YAML"))
        }
        "python" => {
            Some(serde_yaml::from_str(PYTHON_SPEC).expect("bundled python.yaml must be valid YAML"))
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
        assert!(language_spec("cobol").is_none(), "cobol must be unknown");
        assert!(language_spec("").is_none(), "empty string must be unknown");
    }

    #[test]
    fn python_spec_loads_correctly() {
        let spec = language_spec("python").expect("python spec must load");
        assert!(
            !spec.prompt_guidance.is_empty(),
            "python spec must have prompt guidance"
        );
        assert!(
            !spec.init.commands.is_empty(),
            "python spec must have at least one init command"
        );
        assert!(
            !spec.validation.commands.is_empty(),
            "python spec must have at least one validation command"
        );
    }

    #[test]
    fn python_init_first_command_is_uv_init_vcs_none() {
        let spec = language_spec("python").expect("python spec must load");
        assert!(
            spec.init.commands.len() >= 2,
            "python init must have at least two commands; got: {:?}",
            spec.init.commands
        );
        let cmd = &spec.init.commands[0];
        assert_eq!(cmd.program, "uv", "first init program must be uv");
        assert_eq!(
            cmd.args,
            vec!["init", "--vcs", "none"],
            "first init args must be [init, --vcs, none]; got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn python_init_second_command_adds_dev_dependencies() {
        let spec = language_spec("python").expect("python spec must load");
        let cmd = &spec.init.commands[1];
        assert_eq!(cmd.program, "uv", "second init program must be uv");
        assert!(
            cmd.args.contains(&"add".to_string()),
            "second init args must include 'add'; got: {:?}",
            cmd.args
        );
        assert!(
            cmd.args.contains(&"--dev".to_string()),
            "second init must pass --dev; got: {:?}",
            cmd.args
        );
        for pkg in ["pytest", "ruff", "pyright"] {
            assert!(
                cmd.args.contains(&pkg.to_string()),
                "second init must add {pkg}; got: {:?}",
                cmd.args
            );
        }
    }

    #[test]
    fn python_validation_contains_ruff_pyright_pytest() {
        let spec = language_spec("python").expect("python spec must load");
        let cmds = &spec.validation.commands;

        assert!(
            cmds.iter().all(|c| c.program == "uv"),
            "all python validation commands must use uv; got: {cmds:?}"
        );

        let has_ruff = cmds
            .iter()
            .any(|c| c.args.contains(&"ruff".to_string()) && c.args.contains(&"check".to_string()));
        assert!(
            has_ruff,
            "validation must include ruff check; got: {cmds:?}"
        );

        let has_pyright = cmds.iter().any(|c| c.args.contains(&"pyright".to_string()));
        assert!(
            has_pyright,
            "validation must include pyright; got: {cmds:?}"
        );

        let has_pytest = cmds.iter().any(|c| c.args.contains(&"pytest".to_string()));
        assert!(has_pytest, "validation must include pytest; got: {cmds:?}");
    }
}
