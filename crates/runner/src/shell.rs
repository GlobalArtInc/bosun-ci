use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use bosun_types::ir::Shell;

const PLACEHOLDER: &str = "{0}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invocation {
    words: Vec<String>,
    extension: &'static str,
}

impl Invocation {
    pub(crate) fn resolve(
        shell: Option<&Shell>,
        path: Option<&OsStr>,
    ) -> Result<Invocation, String> {
        match shell {
            None if on_path("bash", path) => Invocation::template("bash -e {0}"),
            None => Invocation::template("sh -e {0}"),
            Some(Shell::Bash) => Invocation::template("bash --noprofile --norc -eo pipefail {0}"),
            Some(Shell::Sh) => Invocation::template("sh -e {0}"),
            Some(Shell::Python) => Invocation::template("python {0}"),
            Some(Shell::Pwsh) => Invocation::template("pwsh -command \". '{0}'\""),
            Some(Shell::Custom(template)) => Invocation::template(template),
        }
    }

    fn template(template: &str) -> Result<Invocation, String> {
        if !template.contains(PLACEHOLDER) {
            return Err(format!(
                "the shell `{template}` has no `{PLACEHOLDER}` for the script"
            ));
        }
        let words = split(template);
        let Some(program) = words.first() else {
            return Err(format!("the shell `{template}` names no program"));
        };
        let extension = match program.to_ascii_lowercase().as_str() {
            "bash" | "sh" => ".sh",
            "python" => ".py",
            "pwsh" | "powershell" => ".ps1",
            "cmd" => ".cmd",
            _ => "",
        };
        Ok(Invocation { words, extension })
    }

    pub(crate) fn extension(&self) -> &'static str {
        self.extension
    }

    pub(crate) fn program(&self, script: &Path) -> String {
        substitute(&self.words[0], script)
    }

    pub(crate) fn arguments(&self, script: &Path) -> Vec<String> {
        self.words[1..]
            .iter()
            .map(|word| substitute(word, script))
            .collect()
    }
}

fn substitute(word: &str, script: &Path) -> String {
    word.replace(PLACEHOLDER, &script.to_string_lossy())
}

fn split(template: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut characters = template.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&'"') => {
                word.push('"');
                characters.next();
                started = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            character => {
                word.push(character);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

fn on_path(program: &str, path: Option<&OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    env::split_paths(path).any(|directory| {
        directory
            .join(program)
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use bosun_types::ir::Shell;

    use super::Invocation;

    const SCRIPT: &str = "/work/_temp/step-1.sh";

    fn words(
        shell: Option<&Shell>,
        path: Option<&OsString>,
    ) -> (String, Vec<String>, &'static str) {
        let invocation = Invocation::resolve(shell, path.map(OsString::as_os_str))
            .unwrap_or_else(|error| panic!("{shell:?}: {error}"));
        let script = Path::new(SCRIPT);
        (
            invocation.program(script),
            invocation.arguments(script),
            invocation.extension(),
        )
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn named_shells_follow_the_templates_of_the_target() {
        assert_eq!(
            words(Some(&Shell::Bash), None),
            (
                "bash".to_owned(),
                strings(&["--noprofile", "--norc", "-eo", "pipefail", SCRIPT]),
                ".sh"
            )
        );
        assert_eq!(
            words(Some(&Shell::Sh), None),
            ("sh".to_owned(), strings(&["-e", SCRIPT]), ".sh")
        );
        assert_eq!(
            words(Some(&Shell::Python), None),
            ("python".to_owned(), strings(&[SCRIPT]), ".py")
        );
        assert_eq!(
            words(Some(&Shell::Pwsh), None),
            (
                "pwsh".to_owned(),
                strings(&["-command", &format!(". '{SCRIPT}'")]),
                ".ps1"
            )
        );
    }

    #[test]
    fn a_custom_template_names_the_program_first_and_groups_double_quoted_words() {
        let custom = Shell::Custom(r#"perl -w "{0}" "a \"b\" c" {0}.x"#.to_owned());
        assert_eq!(
            words(Some(&custom), None),
            (
                "perl".to_owned(),
                strings(&["-w", SCRIPT, "a \"b\" c", &format!("{SCRIPT}.x")]),
                ""
            )
        );
        let refused = Invocation::resolve(Some(&Shell::Custom("perl -w".to_owned())), None);
        assert_eq!(
            refused,
            Err("the shell `perl -w` has no `{0}` for the script".to_owned())
        );
    }

    #[test]
    fn the_script_takes_the_extension_the_target_gives_the_program_as_written() {
        for (shell, extension) in [
            (Shell::Bash, ".sh"),
            (Shell::Sh, ".sh"),
            (Shell::Python, ".py"),
            (Shell::Pwsh, ".ps1"),
        ] {
            assert_eq!(words(Some(&shell), None).2, extension, "{shell:?}");
        }
        for (template, extension) in [
            ("bash {0}", ".sh"),
            ("sh -c {0}", ".sh"),
            ("python {0}", ".py"),
            ("Python {0}", ".py"),
            ("python3 {0}", ""),
            ("/usr/bin/python {0}", ""),
            ("pwsh -file {0}", ".ps1"),
            ("powershell -command \". '{0}'\"", ".ps1"),
            ("cmd /c {0}", ".cmd"),
            ("/bin/bash -e {0}", ""),
            ("perl {0}", ""),
        ] {
            let custom = Shell::Custom(template.to_owned());
            assert_eq!(words(Some(&custom), None).2, extension, "{template}");
        }
    }

    #[test]
    fn the_default_shell_is_bash_when_bash_is_on_the_path_and_sh_otherwise() {
        let directory = tempfile::tempdir().expect("a directory");
        let with_bash = directory.path().join("with-bash");
        let without_bash = directory.path().join("without-bash");
        fs::create_dir(&with_bash).expect("a directory");
        fs::create_dir(&without_bash).expect("a directory");
        let bash = with_bash.join("bash");
        fs::write(&bash, "").expect("a file");
        fs::set_permissions(&bash, fs::Permissions::from_mode(0o755)).expect("permissions");
        symlink("/bin/sh", without_bash.join("sh")).expect("a link");
        let not_executable = without_bash.join("bash");
        fs::write(&not_executable, "").expect("a file");
        fs::set_permissions(&not_executable, fs::Permissions::from_mode(0o644))
            .expect("permissions");

        let path = std::env::join_paths([&without_bash, &with_bash]).expect("a PATH");
        assert_eq!(
            words(None, Some(&path)),
            ("bash".to_owned(), strings(&["-e", SCRIPT]), ".sh")
        );
        let path = std::env::join_paths([&without_bash]).expect("a PATH");
        assert_eq!(
            words(None, Some(&path)),
            ("sh".to_owned(), strings(&["-e", SCRIPT]), ".sh")
        );
        assert_eq!(
            words(None, None),
            ("sh".to_owned(), strings(&["-e", SCRIPT]), ".sh")
        );
    }
}
