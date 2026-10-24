use bosun_testkit::TestDatabase;

use crate::{Launch, errors, eventually};

#[tokio::test]
async fn variables_override_the_configuration_file() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url())
        .config(
            "[server]\nlisten = \"127.0.0.1:1\"\n\n[database]\n\
             url = \"postgres://nobody@127.0.0.1:1/nowhere\"\nmax_connections = 2\n",
        )
        .env("BOSUN_SERVER_LISTEN", "127.0.0.1:0")
        .spawn();
    let address = server.listening().await;
    let body = eventually(address, "/readyz", 200).await;
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn a_configuration_the_server_cannot_use_is_refused_with_the_setting_at_fault() {
    for (config, variable, expected) in [
        (
            "[database]\nurll = \"postgres://bosun@db/bosun\"\n",
            None,
            "unknown field `urll`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            Some(("BOSUN_DATABASE_URL", None)),
            "the setting database.url (BOSUN_DATABASE_URL) is not set",
        ),
        (
            "[server]\nlisten = \"everywhere\"\n",
            None,
            "the setting server.listen (BOSUN_SERVER_LISTEN) is `everywhere`",
        ),
        (
            "[database]\nmax_connections = 1\n",
            None,
            "the setting database.max_connections (BOSUN_DATABASE_MAX_CONNECTIONS) is 1",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            Some(("BOSUN_DATABASE_MAX_CONNECTIONS", Some("many"))),
            "the setting database.max_connections (BOSUN_DATABASE_MAX_CONNECTIONS) is `many`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            Some(("BOSUN_BOOTSTRAP_ADMIN", Some("Roman X"))),
            "the setting bootstrap.admin (BOSUN_BOOTSTRAP_ADMIN) is `Roman X`, which is not a \
             login: a login is 1 to 64",
        ),
        (
            "[bootstrap]\nadmin = \"\"\n",
            None,
            "the setting bootstrap.admin (BOSUN_BOOTSTRAP_ADMIN) is ``, which is not a login",
        ),
    ] {
        let mut launch = Launch::serve("postgres://bosun@127.0.0.1:1/bosun").config(config);
        launch = match variable {
            Some((name, Some(value))) => launch.env(name, value),
            Some((name, None)) => launch.env_remove(name),
            None => launch,
        };
        let path = launch.config_path();
        let (status, lines) = launch.spawn().exit().await;
        assert!(!status.success(), "{config}: {lines:#?}");
        let errors = errors(&lines);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "expected an error containing `{expected}`: {lines:#?}"
        );
        if expected.starts_with("unknown field") {
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains(&path.display().to_string())),
                "the error does not name the file: {lines:#?}"
            );
        }
    }
}

#[tokio::test]
async fn an_explicit_configuration_file_that_does_not_exist_is_refused() {
    let launch = Launch::serve("postgres://bosun@127.0.0.1:1/bosun");
    let path = launch.config_path();
    std::fs::remove_file(&path).expect("the configuration file goes away");
    let (status, lines) = launch.spawn().exit().await;
    assert!(!status.success(), "{lines:#?}");
    assert!(
        errors(&lines).iter().any(|error| error.contains(&format!(
            "could not read the configuration {}",
            path.display()
        ))),
        "{lines:#?}"
    );
}
