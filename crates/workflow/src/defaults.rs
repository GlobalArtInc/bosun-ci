use bosun_expr::Key;
use bosun_types::ir::{Defaults, Evaluable, Shell, Template};

use crate::Code;
use crate::expression::OPEN;
use crate::lower::{Entry, Lowering, optional};
use crate::surface::{DEFAULTS, DEFAULTS_RUN};

impl Lowering<'_> {
    pub(crate) fn defaults(
        &mut self,
        entry: &Entry<'_>,
        place: &str,
        key: Option<Key>,
    ) -> Option<Defaults> {
        let what = format!("`defaults` of {place}");
        let fields = self.fields(entry.value, &DEFAULTS, &what, &entry.path, entry.key.span)?;
        let Some(run) = fields.get("run") else {
            return fields.complete.then(Defaults::default);
        };
        let what = format!("`defaults.run` of {place}");
        let run_fields = self.fields(run.value, &DEFAULTS_RUN, &what, &run.path, run.key.span)?;
        let shell = optional(run_fields.get("shell"), |field| {
            self.default_shell(field, &what, key)
        });
        let working_directory = optional(run_fields.get("working-directory"), |field| {
            self.default_directory(field, &what, key)
        });
        if !fields.complete || !run_fields.complete {
            return None;
        }
        Some(Defaults {
            shell: shell?,
            working_directory: working_directory?,
        })
    }

    fn default_shell(
        &mut self,
        field: &Entry<'_>,
        what: &str,
        key: Option<Key>,
    ) -> Option<Evaluable<Shell>> {
        let what = format!("`shell` of {what}");
        let text = self.string(field.value, &field.path, &what)?;
        if let Some(key) = key
            && text.contains(OPEN)
        {
            return self
                .parse_template(&text, field.value, &field.path, key)
                .map(Evaluable::Template);
        }
        let text = self.literal(text, field.value, &field.path, &what)?;
        self.shell_named(&text, field, &what)
            .map(Evaluable::Literal)
    }

    fn default_directory(
        &mut self,
        field: &Entry<'_>,
        what: &str,
        key: Option<Key>,
    ) -> Option<Template> {
        let what = format!("`working-directory` of {what}");
        match key {
            Some(key) => self.template(field.value, &field.path, &what, key),
            None => {
                let text = self.string(field.value, &field.path, &what)?;
                self.literal(text, field.value, &field.path, &what)
                    .map(Template::text)
            }
        }
    }

    pub(crate) fn shell_named(
        &mut self,
        text: &str,
        field: &Entry<'_>,
        what: &str,
    ) -> Option<Shell> {
        match text.parse::<Shell>() {
            Ok(shell) => Some(shell),
            Err(error) => {
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("{what} is {error}"),
                );
                None
            }
        }
    }
}
