use std::collections::BTreeSet;

use bosun_expr::{Key, Side};
use serde_json::{Value, json};

use crate::cases::AVAILABILITY;
use crate::{fixtures, load};

pub fn side(side: Side) -> &'static str {
    match side {
        Side::Server => "server",
        Side::Agent => "agent",
    }
}

fn table() -> Value {
    Key::ALL
        .iter()
        .map(|key| {
            json!({
                "key": key.path(),
                "contexts": key.contexts().iter().map(|context| context.name()).collect::<Vec<_>>(),
                "special_functions": key
                    .special_functions()
                    .iter()
                    .map(|function| function.name())
                    .collect::<Vec<_>>(),
                "evaluated_by": side(key.side()),
            })
        })
        .collect()
}

#[test]
fn the_availability_fixture_equals_the_table_in_the_crate() {
    let committed = load(&fixtures().join(AVAILABILITY));
    assert_eq!(
        table(),
        committed,
        "the availability table differs from expr/{AVAILABILITY}; it is now\n{}",
        serde_json::to_string_pretty(&table()).expect("serialisable")
    );
}

#[test]
fn every_key_sees_bosun_wherever_it_sees_github() {
    let paths: BTreeSet<&str> = Key::ALL.iter().map(|key| key.path()).collect();
    assert_eq!(paths.len(), Key::ALL.len(), "two keys share a path");
    for key in Key::ALL {
        let names: Vec<&str> = key
            .contexts()
            .iter()
            .map(|context| context.name())
            .collect();
        assert_eq!(
            names.contains(&"github"),
            names.contains(&"bosun"),
            "`{}`",
            key.path()
        );
    }
}
