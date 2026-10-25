use bosun_expr::Code;

fn code(text: &str) -> Option<Code> {
    bosun_expr::expression(text).err().map(|error| error.code)
}

#[test]
fn an_expression_has_at_most_21000_characters() {
    let longest = format!("'{}'", "a".repeat(20_998));
    assert_eq!(longest.chars().count(), 21_000);
    assert_eq!(code(&longest), None);
    assert_eq!(code(&format!("  {longest}\n")), None);
    assert_eq!(code(&format!("'{}'", "é".repeat(20_998))), None);
    assert_eq!(
        code(&format!("'{}'", "a".repeat(20_999))),
        Some(Code::LimitExceeded)
    );
    assert_eq!(code(&format!("{longest} == 1")), Some(Code::LimitExceeded));
}

#[test]
fn an_expression_nests_at_most_50_levels() {
    type Build = fn(usize) -> String;
    let builds: [(&str, Build); 8] = [
        ("groups", |n| format!("{}a{}", "(".repeat(n), ")".repeat(n))),
        ("nots", |n| format!("{}a", "!".repeat(n))),
        ("properties", |n| format!("a{}", ".b".repeat(n))),
        ("indexes", |n| format!("a{}", "[0]".repeat(n))),
        ("filters", |n| format!("a{}", ".*".repeat(n))),
        ("calls", |n| format!("{}a{}", "f(".repeat(n), ")".repeat(n))),
        ("comparisons", |n| format!("a{}", " == a".repeat(n))),
        ("arguments", |n| {
            format!("{}a{}", "f(1, ".repeat(n), ")".repeat(n))
        }),
    ];
    for (name, build) in builds {
        assert_eq!(code(&build(49)), None, "{name}: 50 levels are allowed");
        assert_eq!(
            code(&build(50)),
            Some(Code::LimitExceeded),
            "{name}: 51 levels are refused"
        );
    }
    let mixed = |n: usize| format!("{}a{}", "!(".repeat(n), ")".repeat(n));
    assert_eq!(code(&mixed(24)), None);
    assert_eq!(code(&mixed(25)), Some(Code::LimitExceeded));
}

#[test]
fn a_chain_of_and_or_or_is_one_level() {
    for operator in ["&&", "||"] {
        let chain = format!("a{}", format!(" {operator} a").repeat(1_000));
        assert_eq!(code(&chain), None, "{operator}");
    }
}

#[test]
fn deep_nesting_is_refused_without_exhausting_the_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            [
                format!("{}a", "(".repeat(20_000)),
                "!".repeat(20_000),
                format!("{}a", "f(".repeat(10_000)),
                format!("a{}", "[a".repeat(10_000)),
            ]
            .iter()
            .map(|text| code(text))
            .collect::<Vec<_>>()
        })
        .expect("a thread starts");
    let codes = handle
        .join()
        .expect("the parser does not overflow its stack");
    assert_eq!(codes, vec![Some(Code::LimitExceeded); 4]);
}
