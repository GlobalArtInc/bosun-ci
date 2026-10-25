use std::collections::BTreeMap;

use bosun_db::runs::{NewMember, Strategy};
use bosun_expr::{Scope, Value};
use bosun_types::ir::{self, Evaluable, JobTemplate, MatrixField, MatrixValue, Segment, Template};

use super::{Fault, INVALID_TYPE, INVALID_VALUE, Place, at, cover, described};

const MOST_MEMBERS: usize = 256;

const MOST_COMBINATIONS: usize = MOST_MEMBERS * MOST_MEMBERS;

const LIMIT_EXCEEDED: &str = "limit_exceeded";

pub(super) enum Expansion {
    Members(Vec<NewMember>),
    Single(Strategy),
}

type Combination = Vec<(String, Value)>;

struct Matrix {
    vectors: Vec<(String, Vec<Value>, Place)>,
    include: Vec<Combination>,
    exclude: Vec<Combination>,
    place: Place,
}

struct Settings {
    matrix: Option<Matrix>,
    fail_fast: bool,
    max_parallel: Option<u32>,
}

pub(super) fn expand(
    job: &JobTemplate,
    strategy: &Evaluable<ir::Strategy>,
    scope: &dyn Scope,
) -> Result<Expansion, Fault> {
    let settings = match strategy {
        Evaluable::Template(template) => {
            let place = Place::of(job, "strategy", &["strategy"], template);
            let value = place.typed(template, scope)?;
            computed(job, &place, value)?
        }
        Evaluable::Literal(strategy) => written(job, strategy, scope)?,
    };
    let Some(matrix) = settings.matrix else {
        return Ok(Expansion::Single(Strategy {
            fail_fast: settings.fail_fast,
            job_index: 0,
            job_total: 1,
            max_parallel: settings.max_parallel.unwrap_or(1),
        }));
    };
    let combinations = combine(&matrix)?;
    let total = combinations.len();
    if total > MOST_MEMBERS {
        return Err(matrix.place.refuse(
            LIMIT_EXCEEDED,
            format!("expands to {total} jobs, and a matrix expands to at most {MOST_MEMBERS}"),
        ));
    }
    let job_total = whole(total);
    let base = job
        .name
        .as_ref()
        .and_then(Template::as_literal)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| job.key.to_string());
    let members = combinations
        .into_iter()
        .enumerate()
        .map(|(index, combination)| {
            let mut values = Vec::new();
            for (_, value) in &combination {
                leaves(value, &mut values);
            }
            let name = if values.is_empty() {
                base.clone()
            } else {
                format!("{base} ({})", values.join(", "))
            };
            NewMember {
                name,
                matrix: combination
                    .iter()
                    .map(|(name, value)| (name.clone(), json(value)))
                    .collect(),
                strategy: Strategy {
                    fail_fast: settings.fail_fast,
                    job_index: whole(index),
                    job_total,
                    max_parallel: settings.max_parallel.unwrap_or(job_total),
                },
            }
        })
        .collect();
    Ok(Expansion::Members(members))
}

fn whole(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn computed(job: &JobTemplate, place: &Place, value: Value) -> Result<Settings, Fault> {
    let Value::Object(fields) = value else {
        return Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be a mapping, and its expression gives {}",
                described(&value)
            ),
        ));
    };
    let mut settings = Settings {
        matrix: None,
        fail_fast: true,
        max_parallel: None,
    };
    for (field, value) in fields.iter() {
        let place = Place {
            what: format!("`strategy.{field}` of job `{}`", job.key),
            path: at(&job.key, &["strategy", field]),
            span: place.span,
        };
        match field.as_str() {
            "matrix" => settings.matrix = Some(computed_matrix(job, &place, value.clone())?),
            "fail-fast" => settings.fail_fast = flag(&place, value)?,
            "max-parallel" => settings.max_parallel = Some(limit(&place, value)?),
            _ => {
                return Err(place.refuse(
                    INVALID_VALUE,
                    "is no key of a strategy, which takes `matrix`, `fail-fast` and \
                     `max-parallel`"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(settings)
}

fn computed_matrix(job: &JobTemplate, place: &Place, value: Value) -> Result<Matrix, Fault> {
    let Value::Object(fields) = value else {
        return Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be a mapping, and its expression gives {}",
                described(&value)
            ),
        ));
    };
    let mut matrix = Matrix {
        vectors: Vec::new(),
        include: Vec::new(),
        exclude: Vec::new(),
        place: place.clone(),
    };
    for (field, value) in fields.iter() {
        let place = Place {
            what: format!("`strategy.matrix.{field}` of job `{}`", job.key),
            path: at(&job.key, &["strategy", "matrix", field]),
            span: place.span,
        };
        match field.as_str() {
            "include" => matrix.include = entries(&place, value.clone())?,
            "exclude" => matrix.exclude = entries(&place, value.clone())?,
            _ => {
                let values = values(&place, value.clone())?;
                matrix.vectors.push((field.clone(), values, place));
            }
        }
    }
    Ok(matrix)
}

fn written(
    job: &JobTemplate,
    strategy: &ir::Strategy,
    scope: &dyn Scope,
) -> Result<Settings, Fault> {
    let matrix = match &strategy.matrix {
        None => None,
        Some(Evaluable::Template(template)) => {
            let place = Place::of(job, "strategy.matrix", &["strategy", "matrix"], template);
            let value = place.typed(template, scope)?;
            Some(computed_matrix(job, &place, value)?)
        }
        Some(Evaluable::Literal(matrix)) => Some(written_matrix(job, matrix, scope)?),
    };
    let fail_fast = match &strategy.fail_fast {
        Evaluable::Literal(flag) => *flag,
        Evaluable::Template(template) => {
            let place = Place::of(
                job,
                "strategy.fail-fast",
                &["strategy", "fail-fast"],
                template,
            );
            let value = place.typed(template, scope)?;
            flag(&place, &value)?
        }
    };
    let max_parallel = match &strategy.max_parallel {
        None => None,
        Some(Evaluable::Literal(most)) => Some(*most),
        Some(Evaluable::Template(template)) => {
            let place = Place::of(
                job,
                "strategy.max-parallel",
                &["strategy", "max-parallel"],
                template,
            );
            let value = place.typed(template, scope)?;
            Some(limit(&place, &value)?)
        }
    };
    Ok(Settings {
        matrix,
        fail_fast,
        max_parallel,
    })
}

fn written_matrix(
    job: &JobTemplate,
    matrix: &ir::Matrix,
    scope: &dyn Scope,
) -> Result<Matrix, Fault> {
    let mut templates = Vec::new();
    for vector in &matrix.vectors {
        evaluable_templates(&vector.values, &mut templates);
    }
    let place = Place::new(job, "strategy.matrix", &["strategy", "matrix"], &templates);
    let mut found = Matrix {
        vectors: Vec::new(),
        include: Vec::new(),
        exclude: Vec::new(),
        place,
    };
    for vector in &matrix.vectors {
        let name = vector.name.as_str();
        let mut templates = Vec::new();
        evaluable_templates(&vector.values, &mut templates);
        let place = Place::new(
            job,
            &format!("strategy.matrix.{name}"),
            &["strategy", "matrix", name],
            &templates,
        );
        let values = match &vector.values {
            Evaluable::Template(template) => values(&place, place.typed(template, scope)?)?,
            Evaluable::Literal(items) => items
                .iter()
                .map(|item| literal(&place, item, scope))
                .collect::<Result<Vec<_>, _>>()?,
        };
        found.vectors.push((name.to_owned(), values, place));
    }
    for (name, combinations, target) in [
        ("include", &matrix.include, &mut found.include),
        ("exclude", &matrix.exclude, &mut found.exclude),
    ] {
        let Some(combinations) = combinations else {
            continue;
        };
        let mut templates = Vec::new();
        match combinations {
            Evaluable::Template(template) => templates.push(template),
            Evaluable::Literal(entries) => {
                for field in entries.iter().flatten() {
                    value_templates(&field.value, &mut templates);
                }
            }
        }
        let place = Place::new(
            job,
            &format!("strategy.matrix.{name}"),
            &["strategy", "matrix", name],
            &templates,
        );
        *target = match combinations {
            Evaluable::Template(template) => entries(&place, place.typed(template, scope)?)?,
            Evaluable::Literal(entries) => entries
                .iter()
                .map(|entry| fields(&place, entry, scope))
                .collect::<Result<Vec<_>, _>>()?,
        };
    }
    Ok(found)
}

fn evaluable_templates<'a>(values: &'a Evaluable<Vec<MatrixValue>>, found: &mut Vec<&'a Template>) {
    match values {
        Evaluable::Template(template) => found.push(template),
        Evaluable::Literal(items) => {
            for item in items {
                value_templates(item, found);
            }
        }
    }
}

fn value_templates<'a>(value: &'a MatrixValue, found: &mut Vec<&'a Template>) {
    match value {
        MatrixValue::String(template)
            if template
                .segments()
                .iter()
                .any(|segment| matches!(segment, Segment::Expression(_))) =>
        {
            found.push(template);
        }
        MatrixValue::List(items) => {
            for item in items {
                value_templates(item, found);
            }
        }
        MatrixValue::Object(fields) => {
            for field in fields {
                value_templates(&field.value, found);
            }
        }
        MatrixValue::Null
        | MatrixValue::Boolean(_)
        | MatrixValue::Number(_)
        | MatrixValue::String(_) => {}
    }
}

fn literal(place: &Place, value: &MatrixValue, scope: &dyn Scope) -> Result<Value, Fault> {
    Ok(match value {
        MatrixValue::Null => Value::Null,
        MatrixValue::Boolean(flag) => Value::Boolean(*flag),
        MatrixValue::Number(number) => Value::Number(number.0),
        MatrixValue::String(template) => {
            let place = Place {
                span: cover(&[template]).unwrap_or(place.span),
                ..place.clone()
            };
            place.typed(template, scope)?
        }
        MatrixValue::List(items) => Value::Array(
            items
                .iter()
                .map(|item| literal(place, item, scope))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        MatrixValue::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|field| Ok((field.name.clone(), literal(place, &field.value, scope)?)))
                .collect::<Result<Vec<_>, Fault>>()?
                .into(),
        ),
    })
}

fn fields(place: &Place, entry: &[MatrixField], scope: &dyn Scope) -> Result<Combination, Fault> {
    entry
        .iter()
        .map(|field| Ok((field.name.clone(), literal(place, &field.value, scope)?)))
        .collect()
}

fn values(place: &Place, value: Value) -> Result<Vec<Value>, Fault> {
    match value {
        Value::Array(items) => Ok(items.to_vec()),
        other => Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be a list, and its expression gives {}",
                described(&other)
            ),
        )),
    }
}

fn entries(place: &Place, value: Value) -> Result<Vec<Combination>, Fault> {
    let refused = |what: &Value| {
        place.refuse(
            INVALID_TYPE,
            format!(
                "must be a list of mappings, and its expression gives {}",
                described(what)
            ),
        )
    };
    let Value::Array(items) = &value else {
        return Err(refused(&value));
    };
    items
        .iter()
        .map(|item| match item {
            Value::Object(fields) => Ok(fields.to_vec()),
            other => Err(refused(other)),
        })
        .collect()
}

fn flag(place: &Place, value: &Value) -> Result<bool, Fault> {
    match value {
        Value::Boolean(flag) => Ok(*flag),
        other => Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be `true` or `false`, and its expression gives {}",
                described(other)
            ),
        )),
    }
}

fn limit(place: &Place, value: &Value) -> Result<u32, Fault> {
    match value {
        Value::Number(most)
            if most.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(most) =>
        {
            Ok(*most as u32)
        }
        Value::Number(most) => Err(place.refuse(
            INVALID_VALUE,
            format!("must be a whole number of at least 1, and its expression gives {most}"),
        )),
        other => Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be a number, and its expression gives {}",
                described(other)
            ),
        )),
    }
}

fn combine(matrix: &Matrix) -> Result<Vec<Combination>, Fault> {
    if let Some((_, _, place)) = matrix
        .vectors
        .iter()
        .find(|(_, values, _)| values.is_empty())
    {
        return Err(place.refuse(
            INVALID_VALUE,
            "is an empty vector, and every vector of a matrix needs a value".to_owned(),
        ));
    }
    if matrix.vectors.is_empty() && matrix.include.is_empty() {
        return Err(matrix.place.refuse(
            INVALID_VALUE,
            "is empty, and a matrix needs a vector or an `include`".to_owned(),
        ));
    }
    let vector = |name: &str| matrix.vectors.iter().any(|(vector, ..)| vector == name);
    for (position, entry) in matrix.exclude.iter().enumerate() {
        if let Some((name, _)) = entry.iter().find(|(name, _)| !vector(name)) {
            return Err(matrix.place.refuse(
                INVALID_VALUE,
                format!(
                    "names `{name}` in entry {} of `exclude`, which is no vector of the matrix",
                    position + 1
                ),
            ));
        }
    }
    let product = matrix
        .vectors
        .iter()
        .try_fold(1_usize, |product, (_, values, _)| {
            product.checked_mul(values.len())
        });
    if product.is_none_or(|product| product > MOST_COMBINATIONS) {
        return Err(matrix.place.refuse(
            LIMIT_EXCEEDED,
            format!(
                "has more than {MOST_COMBINATIONS} combinations of its vectors, and a matrix \
                 expands to at most {MOST_MEMBERS} jobs"
            ),
        ));
    }
    let mut combinations: Vec<Combination> = if matrix.vectors.is_empty() {
        Vec::new()
    } else {
        vec![Vec::new()]
    };
    for (name, values, _) in &matrix.vectors {
        combinations = combinations
            .iter()
            .flat_map(|combination| {
                values.iter().map(move |value| {
                    let mut extended = combination.clone();
                    extended.push((name.clone(), value.clone()));
                    extended
                })
            })
            .collect();
    }
    combinations.retain(|combination| {
        !matrix
            .exclude
            .iter()
            .any(|entry| entry.iter().all(|field| holds(combination, field)))
    });
    let originals = combinations.len();
    for entry in &matrix.include {
        let mut matched = false;
        for combination in &mut combinations[..originals] {
            let fits = entry
                .iter()
                .filter(|(name, _)| vector(name))
                .all(|field| holds(combination, field));
            if !fits {
                continue;
            }
            matched = true;
            for (name, value) in entry.iter().filter(|(name, _)| !vector(name)) {
                match combination.iter_mut().find(|(present, _)| present == name) {
                    Some((_, present)) => *present = value.clone(),
                    None => combination.push((name.clone(), value.clone())),
                }
            }
        }
        if !matched {
            combinations.push(entry.clone());
        }
    }
    if combinations.is_empty() {
        return Err(matrix.place.refuse(
            INVALID_VALUE,
            "leaves no combination once `exclude` is applied".to_owned(),
        ));
    }
    Ok(combinations)
}

fn holds(combination: &Combination, (name, value): &(String, Value)) -> bool {
    combination
        .iter()
        .any(|(present, held)| present == name && same(held, value))
}

fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right.iter()).all(|(l, r)| same(l, r))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, value)| {
                    right
                        .iter()
                        .any(|(other, held)| other == name && same(value, held))
                })
        }
        _ => left == right,
    }
}

fn leaves(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items.iter() {
                leaves(item, found);
            }
        }
        Value::Object(fields) => {
            for (_, value) in fields.iter() {
                leaves(value, found);
            }
        }
        Value::Null => found.push(String::new()),
        Value::Boolean(flag) => found.push(flag.to_string()),
        Value::Number(number) if *number == 0.0 => found.push("0".to_owned()),
        Value::Number(number) => found.push(number.to_string()),
        Value::String(text) => found.push(text.clone()),
    }
}

fn json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(flag) => serde_json::Value::Bool(*flag),
        Value::Number(number)
            if number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 =>
        {
            serde_json::Value::from(*number as i64)
        }
        Value::Number(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(text) => serde_json::Value::String(text.clone()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(json).collect()),
        Value::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), json(value)))
                .collect(),
        ),
    }
}

pub(super) fn context(
    matrix: &BTreeMap<String, serde_json::Value>,
) -> Result<Value, serde_json::Error> {
    serde_json::from_value(serde_json::to_value(matrix)?)
}
