use std::collections::BTreeMap;

pub(crate) enum Words {
    Fixed(Vec<String>),
    Choice {
        first: Vec<String>,
        second: Vec<String>,
    },
}

pub(crate) struct Launch<'a> {
    pub(crate) remove: &'a str,
    pub(crate) directory: &'a str,
    pub(crate) path: &'a [String],
    pub(crate) environment: &'a BTreeMap<String, String>,
    pub(crate) words: &'a Words,
}

const SHELL_VARIABLES: &[&str] = &[
    "_",
    "BASHPID",
    "EUID",
    "FUNCNAME",
    "GROUPS",
    "IFS",
    "LINENO",
    "OLDPWD",
    "OPTARG",
    "OPTIND",
    "PPID",
    "PWD",
    "RANDOM",
    "SECONDS",
    "SHELLOPTS",
    "SHLVL",
    "UID",
];

fn exportable(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|next| next.is_ascii_alphanumeric() || next == '_')
        && !SHELL_VARIABLES.contains(&name)
        && !name.starts_with("BASH_")
}

pub(crate) fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

pub(crate) fn script(launch: &Launch<'_>) -> Result<String, String> {
    let Launch {
        remove,
        directory,
        path,
        environment,
        words,
    } = launch;
    let mut checked: Vec<&str> = vec![remove, directory];
    checked.extend(path.iter().map(String::as_str));
    match words {
        Words::Fixed(words) => checked.extend(words.iter().map(String::as_str)),
        Words::Choice { first, second } => {
            checked.extend(first.iter().chain(second).map(String::as_str));
        }
    }
    if matches!(words, Words::Fixed(fixed) if fixed.is_empty()) {
        return Err("the step has no program to run".to_owned());
    }
    for (name, value) in environment.iter() {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(format!(
                "the variable `{}` has a name that the environment of a process cannot hold",
                name.escape_debug()
            ));
        }
        checked.push(value);
    }
    if checked.iter().any(|text| text.contains('\0')) {
        return Err("a value of the step's environment holds a NUL character".to_owned());
    }
    let mut script = String::new();
    script.push_str(&format!("rm -f -- {}\n", quote(remove)));
    script.push_str(&format!("cd -- {} || exit 126\n", quote(directory)));
    let own = environment.get("PATH");
    match (path.is_empty(), own) {
        (true, None) => {}
        (true, Some(own)) => script.push_str(&format!("PATH={}\nexport PATH\n", quote(own))),
        (false, own) => {
            let entries = path.join(":");
            let tail = match own {
                Some(own) if !own.is_empty() => format!(":{}", quote(own)),
                Some(_) => String::new(),
                None => "\"${PATH:+:$PATH}\"".to_owned(),
            };
            script.push_str(&format!("PATH={}{tail}\nexport PATH\n", quote(&entries)));
        }
    }
    let command = |words: &[String]| words.iter().map(|word| quote(word)).collect::<Vec<_>>();
    match words {
        Words::Fixed(words) => script.push_str(&format!("set -- {}\n", command(words).join(" "))),
        Words::Choice { first, second } => {
            let Some(program) = first.first() else {
                return Err("the step has no program to run".to_owned());
            };
            script.push_str(&format!(
                "if command -v {} >/dev/null 2>&1; then set -- {}; else set -- {}; fi\n",
                quote(program),
                command(first).join(" "),
                command(second).join(" ")
            ));
        }
    }
    let (exported, others): (Vec<_>, Vec<_>) = environment
        .iter()
        .filter(|(name, _)| name.as_str() != "PATH")
        .partition(|(name, _)| exportable(name));
    for (name, value) in exported {
        script.push_str(&format!("export {name}={}\n", quote(value)));
    }
    if others.is_empty() {
        script.push_str("exec \"$@\"\n");
    } else {
        let assignments: Vec<String> = others
            .into_iter()
            .map(|(name, value)| quote(&format!("{name}={value}")))
            .collect();
        script.push_str(&format!("exec env -- {} \"$@\"\n", assignments.join(" ")));
    }
    Ok(script)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{Launch, Words, quote, script};

    fn run(directory: &Path, launch: &Launch<'_>) -> String {
        let file = directory.join("step.launch");
        fs::write(&file, script(launch).expect("a script")).expect("a file");
        let output = Command::new("sh")
            .arg(&file)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("sh runs");
        assert!(output.status.success(), "{output:#?}");
        String::from_utf8(output.stdout).expect("UTF-8")
    }

    fn words(words: &[&str]) -> Words {
        Words::Fixed(words.iter().map(|word| (*word).to_owned()).collect())
    }

    fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_quoted_text_is_one_word_whatever_it_holds() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn every_variable_reaches_the_program_with_its_name_and_value_untouched() {
        let directory = tempfile::tempdir().expect("a directory");
        let tricky = [
            ("INPUT_NODE-VERSION", "20"),
            ("MULTI", "first\nsecond\n"),
            ("QUOTES", "it's \"quoted\" `x` $HOME \\ ; & | *"),
            ("EMPTY", ""),
            ("dotted.name", "v"),
            ("UID", "not the user id of the shell"),
            ("BASH_ENV", "/nonexistent"),
        ];
        let launcher = directory.path().join("step.launch");
        let launcher = launcher.to_string_lossy().into_owned();
        for (name, value) in tricky {
            let environment = environment(&tricky);
            let program = words(&["printenv", name]);
            let launch = Launch {
                remove: &launcher,
                directory: &directory.path().to_string_lossy(),
                path: &[],
                environment: &environment,
                words: &program,
            };
            let printed = run(directory.path(), &launch);
            assert_eq!(
                printed.trim_end_matches('\n'),
                value.trim_end_matches('\n'),
                "{name}"
            );
        }
    }

    #[test]
    fn only_a_variable_the_shell_cannot_export_becomes_an_argument_of_env() {
        let environment = environment(&[
            ("SECRET", "plain"),
            ("INPUT_GITHUB-TOKEN", "dashed"),
            ("UID", "special"),
        ]);
        let program = words(&["true"]);
        let text = script(&Launch {
            remove: "/l",
            directory: "/d",
            path: &[],
            environment: &environment,
            words: &program,
        })
        .expect("a script");
        assert!(text.contains("export SECRET='plain'\n"), "{text}");
        let started = text.find("exec env -- ").expect("an env");
        let arguments = &text[started..];
        assert!(!arguments.contains("SECRET"), "{text}");
        assert!(arguments.contains("'INPUT_GITHUB-TOKEN=dashed'"), "{text}");
        assert!(arguments.contains("'UID=special'"), "{text}");
    }

    #[test]
    fn the_launcher_removes_itself_changes_directory_and_keeps_the_path_entries_in_front() {
        let directory = tempfile::tempdir().expect("a directory");
        let work = fs::canonicalize(directory.path()).expect("a path");
        let launcher = work.join("step.launch").to_string_lossy().into_owned();
        let entries = ["/first/bin".to_owned(), "/second dir/bin".to_owned()];
        let program = words(&[
            "/bin/sh",
            "-c",
            "pwd -P; printf '%s\\n' \"$PATH\"; test ! -e \"$0\"",
            &launcher,
        ]);
        let none = environment(&[]);
        let printed = run(
            &work,
            &Launch {
                remove: &launcher,
                directory: &work.to_string_lossy(),
                path: &entries,
                environment: &none,
                words: &program,
            },
        );
        assert_eq!(
            printed,
            format!(
                "{}\n/first/bin:/second dir/bin:/usr/bin:/bin\n",
                work.display()
            )
        );
        assert!(!Path::new(&launcher).exists());
        let own = environment(&[("PATH", "/own/bin")]);
        let printed = run(
            &work,
            &Launch {
                remove: &launcher,
                directory: &work.to_string_lossy(),
                path: &entries,
                environment: &own,
                words: &program,
            },
        );
        assert!(
            printed.ends_with("/first/bin:/second dir/bin:/own/bin\n"),
            "{printed}"
        );
        let printed = run(
            &work,
            &Launch {
                remove: &launcher,
                directory: &work.to_string_lossy(),
                path: &[],
                environment: &own,
                words: &program,
            },
        );
        assert!(printed.ends_with("\n/own/bin\n"), "{printed}");
    }

    #[test]
    fn a_choice_takes_the_first_program_when_it_is_on_the_path_and_the_second_otherwise() {
        let directory = tempfile::tempdir().expect("a directory");
        let launcher = directory
            .path()
            .join("step.launch")
            .to_string_lossy()
            .into_owned();
        let none = environment(&[]);
        let owned = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        for (first, expected) in [("sh", "first"), ("a-program-no-image-has", "second")] {
            let choice = Words::Choice {
                first: owned(&[first, "-c", "printf first"]),
                second: owned(&["sh", "-c", "printf second"]),
            };
            let printed = run(
                directory.path(),
                &Launch {
                    remove: &launcher,
                    directory: &directory.path().to_string_lossy(),
                    path: &[],
                    environment: &none,
                    words: &choice,
                },
            );
            assert_eq!(printed, expected);
        }
    }

    #[test]
    fn a_variable_or_a_word_a_process_cannot_hold_is_refused_before_anything_runs() {
        let none = environment(&[]);
        let program = words(&["true"]);
        let launch = |environment: &BTreeMap<String, String>, words: &Words| {
            script(&Launch {
                remove: "/l",
                directory: "/d",
                path: &[],
                environment,
                words,
            })
        };
        for bad in [("A=B", "x"), ("", "x"), ("A\0B", "x"), ("A", "x\0y")] {
            let environment = environment(&[bad]);
            assert!(launch(&environment, &program).is_err(), "{bad:?}");
        }
        assert!(launch(&none, &words(&[])).is_err());
        assert!(launch(&none, &words(&["a\0b"])).is_err());
        assert!(launch(&none, &program).is_ok());
    }
}
