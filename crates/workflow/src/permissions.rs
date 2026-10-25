use std::collections::BTreeMap;

use bosun_types::ir::{Access, Permissions, Scope};

use crate::Code;
use crate::lower::{Entry, Lowering};
use crate::surface::{Arrives, suggest};
use crate::yaml::Value;

const ANY: &[Access] = &[Access::Read, Access::Write, Access::None];
const WRITE_OR_NONE: &[Access] = &[Access::Write, Access::None];
const READ_OR_NONE: &[Access] = &[Access::Read, Access::None];

fn levels(scope: Scope) -> &'static [Access] {
    match scope {
        Scope::IdToken => WRITE_OR_NONE,
        Scope::Models => READ_OR_NONE,
        _ => ANY,
    }
}

fn listed(levels: &[Access]) -> String {
    let names: Vec<String> = levels
        .iter()
        .map(|level| format!("`{}`", level.name()))
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

impl Lowering<'_> {
    pub(crate) fn permissions(&mut self, entry: &Entry<'_>, place: &str) -> Option<Permissions> {
        let what = format!("`permissions` of {place}");
        match &entry.value.value {
            Value::String(text) => match text.as_str() {
                "read-all" => Some(Permissions::ReadAll),
                "write-all" => Some(Permissions::WriteAll),
                _ => {
                    let hint = suggest(text, ["read-all", "write-all"])
                        .map(|form| format!("; did you mean `{form}`?"))
                        .unwrap_or_default();
                    self.error(
                        Code::InvalidValue,
                        entry.value.span,
                        &entry.path,
                        format!(
                            "{what} is `{text}`, which is neither `read-all`, `write-all` nor a mapping of scopes{hint}"
                        ),
                    );
                    None
                }
            },
            Value::Mapping(_) => self.scopes(entry, &what),
            _ => {
                self.error(
                    Code::InvalidType,
                    entry.value.span,
                    &entry.path,
                    format!("{what} must be `read-all`, `write-all` or a mapping of scopes"),
                );
                None
            }
        }
    }

    fn scopes(&mut self, entry: &Entry<'_>, what: &str) -> Option<Permissions> {
        let found = self.entries(entry.value, &entry.path, what)?;
        let mut scopes = BTreeMap::new();
        let mut complete = found.complete;
        for field in &found.entries {
            let Some(scope) = Scope::ALL
                .into_iter()
                .find(|scope| scope.name() == field.name)
            else {
                self.see(field.path.clone(), Arrives::Unknown);
                complete = false;
                let suggestion = suggest(field.name, Scope::ALL.map(Scope::name));
                self.unknown(
                    "scope",
                    field.name,
                    what,
                    suggestion,
                    field.key.span,
                    &field.path,
                );
                continue;
            };
            self.see(field.path.clone(), Arrives::Inherited);
            let accepted = levels(scope);
            let level = match &field.value.value {
                Value::String(text) => accepted
                    .iter()
                    .copied()
                    .find(|level| level.name() == text.as_str()),
                _ => None,
            };
            match level {
                Some(level) => {
                    scopes.insert(scope, level);
                }
                None => {
                    complete = false;
                    self.error(
                        Code::InvalidValue,
                        field.value.span,
                        &field.path,
                        format!(
                            "scope `{}` in {what} must be {}",
                            field.name,
                            listed(accepted)
                        ),
                    );
                }
            }
        }
        complete.then_some(Permissions::Scopes(scopes))
    }
}
