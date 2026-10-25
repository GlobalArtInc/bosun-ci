use std::collections::{BTreeMap, BTreeSet};

use bosun_db::identities::NewIdentity;
use bosun_types::api::User;
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde_json::{Map, Value};
use tracing::{info, warn};

use crate::config::Oidc;
use crate::owner::{self, LOGIN_RULE};

const NAME_CLAIM: &str = "name";

const EMAIL_VERIFIED_CLAIM: &str = "email_verified";

const NAME_LENGTH: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Person {
    pub(super) subject: String,
    pub(super) login: String,
    pub(super) name: String,
    pub(super) email: Option<String>,
    pub(super) email_verified: bool,
    pub(super) groups: BTreeSet<String>,
}

pub(super) enum Refusal {
    Outside(String),
    Taken(String),
    Database(bosun_db::Error),
}

impl From<bosun_db::Error> for Refusal {
    fn from(error: bosun_db::Error) -> Refusal {
        Refusal::Database(error)
    }
}

impl From<sea_orm::DbErr> for Refusal {
    fn from(error: sea_orm::DbErr) -> Refusal {
        Refusal::Database(error.into())
    }
}

pub(super) fn person(claims: &Map<String, Value>, config: &Oidc) -> Result<Person, Refusal> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    let subject = text("sub").unwrap_or_default().to_owned();
    let claimed = text(&config.login_claim).ok_or_else(|| {
        Refusal::Outside(format!(
            "the ID token of `{}` carries no claim `{}`, which names the login",
            config.name, config.login_claim
        ))
    })?;
    let login = claimed.to_ascii_lowercase();
    if !owner::is_login(&login) {
        return Err(Refusal::Outside(format!(
            "the claim `{}` of `{}` is `{claimed}`, which is not a login: {LOGIN_RULE}",
            config.login_claim, config.name
        )));
    }
    let name = text(NAME_CLAIM)
        .map(str::trim)
        .filter(|name| {
            !name.is_empty()
                && name.chars().count() <= NAME_LENGTH
                && !name.chars().any(char::is_control)
        })
        .unwrap_or(&login)
        .to_owned();
    let groups = match claims.get(&config.groups_claim) {
        Some(Value::Array(groups)) => groups
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => BTreeSet::new(),
    };
    Ok(Person {
        subject,
        login,
        name,
        email: text(&config.email_claim).map(str::to_owned),
        email_verified: claims.get(EMAIL_VERIFIED_CLAIM) == Some(&Value::Bool(true)),
        groups,
    })
}

pub(super) fn admitted(person: &Person, config: &Oidc) -> bool {
    let by_group = config
        .allowed_groups
        .iter()
        .any(|group| person.groups.contains(group));
    let by_domain = person.email_verified
        && person
            .email
            .as_deref()
            .and_then(|email| email.rsplit_once('@'))
            .is_some_and(|(_, domain)| {
                config
                    .allowed_email_domains
                    .contains(&domain.to_lowercase())
            });
    by_group || by_domain
}

pub(super) async fn admit(
    db: &DatabaseConnection,
    config: &Oidc,
    person: &Person,
) -> Result<User, Refusal> {
    let transaction = db.begin().await?;
    let user = match bosun_db::identities::find_user(&transaction, &config.issuer, &person.subject)
        .await?
    {
        Some(user) => user,
        None => {
            let created = bosun_db::identities::create_user(
                &transaction,
                NewIdentity {
                    issuer: &config.issuer,
                    subject: &person.subject,
                    login: &person.login,
                    name: &person.name,
                    email: person.email.as_deref(),
                },
            )
            .await;
            match created {
                Ok(user) => {
                    info!(provider = %config.name, login = %user.login, "created a user at its first sign-in through an identity provider");
                    user
                }
                Err(bosun_db::Error::LoginTaken(login)) => {
                    return Err(Refusal::Taken(format!(
                        "the login `{login}` belongs to a user who did not sign in through \
                         `{}`, and a sign-in through a provider never takes over a user by \
                         their login",
                        config.name
                    )));
                }
                Err(error) => return Err(error.into()),
            }
        }
    };
    let mut wanted: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for mapping in &config.teams {
        *wanted
            .entry((mapping.organisation.as_str(), mapping.team.as_str()))
            .or_default() |= person.groups.contains(&mapping.group);
    }
    for ((organisation, team), member) in wanted {
        let Some(found) = bosun_db::organisations::find_by_slug(&transaction, organisation).await?
        else {
            warn!(
                provider = %config.name,
                organisation,
                team,
                "a team that an identity provider maps belongs to an organisation that does not exist"
            );
            continue;
        };
        if member {
            let team = bosun_db::teams::ensure(&transaction, found.id, team).await?;
            bosun_db::teams::add_member(&transaction, team.id, user.id).await?;
        } else if let Some(team) =
            bosun_db::teams::find_by_slug(&transaction, found.id, team).await?
        {
            bosun_db::teams::remove_member(&transaction, team.id, user.id).await?;
        }
    }
    transaction.commit().await?;
    Ok(user)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Map, Value, json};

    use super::{Person, Refusal, admitted, person};
    use crate::config::Oidc;

    fn config() -> Oidc {
        Oidc {
            name: "keycloak".to_owned(),
            issuer: "https://sso.example.org/realms/main".to_owned(),
            client_id: "bosun".to_owned(),
            client_secret_file: PathBuf::from("/etc/bosun/oidc-secret"),
            scopes: vec!["openid".to_owned()],
            login_claim: "preferred_username".to_owned(),
            email_claim: "email".to_owned(),
            groups_claim: "groups".to_owned(),
            allowed_groups: vec!["bosun".to_owned()],
            allowed_email_domains: vec!["globalart.dev".to_owned()],
            teams: Vec::new(),
        }
    }

    fn claims(value: Value) -> Map<String, Value> {
        let Value::Object(claims) = value else {
            panic!("claims are an object");
        };
        claims
    }

    fn read(value: Value) -> Person {
        match person(&claims(value), &config()) {
            Ok(person) => person,
            Err(Refusal::Outside(message) | Refusal::Taken(message)) => panic!("{message}"),
            Err(Refusal::Database(error)) => panic!("{error}"),
        }
    }

    #[test]
    fn the_person_is_read_from_the_configured_claims() {
        let dana = read(json!({
            "sub": "f81d4fae",
            "preferred_username": "Dana",
            "name": "Dana Scully",
            "email": "dana@globalart.dev",
            "email_verified": true,
            "groups": ["bosun", "/ops", 7]
        }));
        assert_eq!(dana.subject, "f81d4fae");
        assert_eq!(dana.login, "dana");
        assert_eq!(dana.name, "Dana Scully");
        assert_eq!(dana.email.as_deref(), Some("dana@globalart.dev"));
        assert!(dana.email_verified);
        assert_eq!(
            dana.groups.iter().map(String::as_str).collect::<Vec<_>>(),
            ["/ops", "bosun"]
        );
        let bare = read(json!({ "sub": "1", "preferred_username": "fox", "name": " " }));
        assert_eq!(bare.name, "fox");
        assert!(bare.groups.is_empty());
        assert!(!bare.email_verified);
        for refused in [
            json!({ "sub": "1" }),
            json!({ "sub": "1", "preferred_username": "Fox Mulder" }),
            json!({ "sub": "1", "preferred_username": "0192f3a4-5b6c-7d8e-9f01-000000000003" }),
        ] {
            assert!(
                matches!(
                    person(&claims(refused.clone()), &config()),
                    Err(Refusal::Outside(_))
                ),
                "{refused}"
            );
        }
    }

    #[test]
    fn a_person_is_admitted_by_a_group_or_by_a_verified_address_of_a_domain() {
        let person = |groups: &[&str], email: &str, verified: bool| Person {
            subject: "1".to_owned(),
            login: "dana".to_owned(),
            name: "Dana".to_owned(),
            email: Some(email.to_owned()),
            email_verified: verified,
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        };
        assert!(admitted(
            &person(&["bosun"], "dana@example.org", false),
            &config()
        ));
        assert!(admitted(
            &person(&[], "dana@GlobalArt.dev", true),
            &config()
        ));
        assert!(!admitted(
            &person(&[], "dana@globalart.dev", false),
            &config()
        ));
        assert!(!admitted(
            &person(&["ops"], "dana@example.org", true),
            &config()
        ));
        assert!(!admitted(
            &person(&[], "dana@globalart.dev.example.org", true),
            &config()
        ));
        let mut closed = config();
        closed.allowed_groups.clear();
        closed.allowed_email_domains.clear();
        assert!(!admitted(
            &person(&["bosun"], "dana@globalart.dev", true),
            &closed
        ));
    }
}
