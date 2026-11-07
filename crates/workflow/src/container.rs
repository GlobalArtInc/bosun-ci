use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Container, Credentials, Service, Span, Template};

use crate::Code;
use crate::lower::{Entry, Lowering, index, optional};
use crate::on::identifier;
use crate::surface::{CONTAINER, CREDENTIALS, SERVICE, Shape};
use crate::yaml::{Node, Value};

struct Keys {
    shape: &'static Shape,
    image: Key,
    rest: Key,
    credentials: Key,
    env: Key,
}

const JOB_CONTAINER: Keys = Keys {
    shape: &CONTAINER,
    image: Key::JobContainerImage,
    rest: Key::JobContainer,
    credentials: Key::JobContainerCredentials,
    env: Key::JobContainerEnv,
};

const JOB_SERVICE: Keys = Keys {
    shape: &SERVICE,
    image: Key::JobServices,
    rest: Key::JobServices,
    credentials: Key::JobServiceCredentials,
    env: Key::JobServiceEnv,
};

impl Lowering<'_> {
    pub(crate) fn container(&mut self, entry: &Entry<'_>, owner: &str) -> Option<Container> {
        let place = format!("`container` of {owner}");
        if let Value::Mapping(_) = entry.value.value {
            return self.container_body(
                entry.value,
                &entry.path,
                entry.key.span,
                &place,
                &JOB_CONTAINER,
            );
        }
        let image = self.image(entry.value, &entry.path, &place, Key::JobContainerImage)?;
        Some(Container {
            image,
            credentials: None,
            env: Default::default(),
            ports: Vec::new(),
            volumes: Vec::new(),
            options: None,
        })
    }

    pub(crate) fn services(&mut self, entry: &Entry<'_>, owner: &str) -> Option<Vec<Service>> {
        let place = format!("`services` of {owner}");
        let found = self.entries(entry.value, &entry.path, &place)?;
        let mut complete = found.complete;
        let mut services = Vec::new();
        for item in &found.entries {
            if !identifier(item.name) {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.key.span,
                    &item.path,
                    format!(
                        "the name of service `{}` of {owner} must start with a letter or `_` and hold only letters, digits, `-` and `_`",
                        item.name
                    ),
                );
                continue;
            }
            let service_place = format!("service `{}` of {owner}", item.name);
            match self.container_body(
                item.value,
                &item.path,
                item.key.span,
                &service_place,
                &JOB_SERVICE,
            ) {
                Some(container) => services.push(Service {
                    name: item.name.to_string(),
                    container,
                }),
                None => complete = false,
            }
        }
        complete.then_some(services)
    }

    fn image(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        place: &str,
        key: Key,
    ) -> Option<Template> {
        let what = format!("the image of {place}");
        let image = self.template(node, path, &what, key)?;
        if image
            .as_literal()
            .is_some_and(|text| text.trim().is_empty())
        {
            self.error(
                Code::InvalidValue,
                node.span,
                path,
                format!("{what} is empty"),
            );
            return None;
        }
        Some(image)
    }

    fn list(&mut self, entry: &Entry<'_>, place: &str, key: Key) -> Option<Vec<Template>> {
        let what = format!("`{}` of {place}", entry.name);
        let Value::Sequence(items) = &entry.value.value else {
            self.error(
                Code::InvalidType,
                entry.value.span,
                &entry.path,
                format!("{what} must be a list"),
            );
            return None;
        };
        let mut templates = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&entry.path, position);
            match self.template(item, &path, &format!("every entry of {what}"), key) {
                Some(template) => templates.push(template),
                None => complete = false,
            }
        }
        complete.then_some(templates)
    }

    fn credentials(&mut self, entry: &Entry<'_>, place: &str, key: Key) -> Option<Credentials> {
        let place = format!("`credentials` of {place}");
        let fields = self.fields(
            entry.value,
            &CREDENTIALS,
            &place,
            &entry.path,
            entry.key.span,
        )?;
        let username = optional(fields.get("username"), |field| {
            self.template(
                field.value,
                &field.path,
                &format!("`username` of {place}"),
                key,
            )
        });
        let password = optional(fields.get("password"), |field| {
            self.template(
                field.value,
                &field.path,
                &format!("`password` of {place}"),
                key,
            )
        });
        let complete = fields.complete;
        let credentials = Credentials {
            username: username?,
            password: password?,
        };
        complete.then_some(credentials)
    }

    fn container_body(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        owner: Span,
        place: &str,
        keys: &Keys,
    ) -> Option<Container> {
        let fields = self.fields(node, keys.shape, place, path, owner)?;
        let image = fields
            .get("image")
            .and_then(|field| self.image(field.value, &field.path, place, keys.image));
        let credentials = optional(fields.get("credentials"), |field| {
            self.credentials(field, place, keys.credentials)
        });
        let env = optional(fields.get("env"), |field| {
            self.templates(
                field.value,
                &field.path,
                &format!("`env` of {place}"),
                "variable",
                keys.env,
            )
        });
        let ports = optional(fields.get("ports"), |field| {
            self.list(field, place, keys.rest)
        });
        let volumes = optional(fields.get("volumes"), |field| {
            self.list(field, place, keys.rest)
        });
        let options = optional(fields.get("options"), |field| {
            self.template(
                field.value,
                &field.path,
                &format!("`options` of {place}"),
                keys.rest,
            )
        });
        if !fields.complete {
            return None;
        }
        Some(Container {
            image: image?,
            credentials: credentials?,
            env: env?.unwrap_or_default(),
            ports: ports?.unwrap_or_default(),
            volumes: volumes?.unwrap_or_default(),
            options: options?,
        })
    }
}
