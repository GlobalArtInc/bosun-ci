use std::cell::RefCell;

use bosun_expr::{Code, Context, Scope, Status, Value};

struct Statuses {
    success: bool,
    failure: bool,
    cancelled: bool,
    asked: RefCell<Vec<&'static str>>,
}

impl Statuses {
    fn new(success: bool, failure: bool, cancelled: bool) -> Statuses {
        Statuses {
            success,
            failure,
            cancelled,
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl Scope for Statuses {
    fn context(&self, context: Context) -> Value {
        self.asked.borrow_mut().push(context.name());
        match context {
            Context::Github => Value::Object(
                vec![(
                    "event_name".to_owned(),
                    Value::String("workflow_dispatch".to_owned()),
                )]
                .into(),
            ),
            _ => Value::Null,
        }
    }

    fn status(&self, status: Status) -> Option<bool> {
        self.asked.borrow_mut().push(match status {
            Status::Success => "success()",
            Status::Failure => "failure()",
            Status::Cancelled => "cancelled()",
        });
        Some(match status {
            Status::Success => self.success,
            Status::Failure => self.failure,
            Status::Cancelled => self.cancelled,
        })
    }
}

struct Plain;

impl Scope for Plain {
    fn context(&self, _: Context) -> Value {
        Value::Null
    }
}

fn holds(text: &str, scope: &Statuses) -> bool {
    let condition = bosun_expr::condition(text)
        .unwrap_or_else(|error| panic!("`{text}` does not parse: {error:?}"))
        .unwrap_or_else(|| panic!("`{text}` sets no condition"));
    bosun_expr::holds(&condition, scope).unwrap_or_else(|error| panic!("`{text}`: {error:?}"))
}

#[test]
fn a_condition_without_a_status_function_needs_success_first() {
    let failed = Statuses::new(false, true, false);
    assert!(!holds("github.event_name == 'workflow_dispatch'", &failed));
    assert_eq!(*failed.asked.borrow(), ["success()"]);
    assert!(!holds("fromJSON('not json')", &failed));

    let succeeded = Statuses::new(true, false, false);
    assert!(holds(
        "github.event_name == 'workflow_dispatch'",
        &succeeded
    ));
    assert_eq!(*succeeded.asked.borrow(), ["success()", "github"]);
    assert!(!holds("${{ github.event_name == 'push' }}", &succeeded));
}

#[test]
fn a_condition_that_calls_a_status_function_stands_alone() {
    let failed = Statuses::new(false, true, false);
    for (text, expected) in [
        ("failure()", true),
        ("FAILURE()", true),
        ("success()", false),
        ("!success()", true),
        ("always()", true),
        ("Always() && github.event_name == 'push'", false),
        ("cancelled()", false),
        ("format('{0}', cancelled()) == 'false'", true),
    ] {
        assert_eq!(holds(text, &failed), expected, "{text}");
    }
    let cancelling = Statuses::new(false, false, true);
    assert!(holds("cancelled()", &cancelling));
    assert!(!holds("failure()", &cancelling));
    assert!(bosun_expr::calls_status(
        &bosun_expr::condition("contains(toJSON(always()), 'true')")
            .expect("a condition")
            .expect("a condition")
    ));
}

#[test]
fn a_template_condition_is_true_unless_it_renders_empty() {
    let succeeded = Statuses::new(true, false, false);
    assert!(holds("${{ false }} == true", &succeeded));
    assert!(!holds("${{ '' }}${{ null }}", &succeeded));
    let failed = Statuses::new(false, true, false);
    assert!(!holds("${{ false }} == true", &failed));
    assert!(holds("${{ always() }} == true", &failed));
}

#[test]
fn the_implicit_success_needs_the_status_functions_of_the_caller() {
    let condition = bosun_expr::condition("github.ref == 'refs/heads/main'")
        .expect("a condition")
        .expect("a condition");
    let error = bosun_expr::holds(&condition, &Plain).expect_err("no status functions");
    assert_eq!(error.code, Code::UnavailableFunction);
    let unknown = bosun_expr::condition("foo.bar")
        .expect("a condition")
        .expect("a condition");
    let error = bosun_expr::holds(&unknown, &Statuses::new(true, false, false))
        .expect_err("an unknown context");
    assert_eq!(error.code, Code::UnknownContext);
}
