use bosun_types::ir::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    ExpressionSyntax,
    LimitExceeded,
    UnknownContext,
    UnavailableContext,
    UnknownFunction,
    UnavailableFunction,
    WrongArgumentCount,
    NotImplemented,
}

impl Code {
    pub const ALL: [Code; 8] = [
        Code::ExpressionSyntax,
        Code::LimitExceeded,
        Code::UnknownContext,
        Code::UnavailableContext,
        Code::UnknownFunction,
        Code::UnavailableFunction,
        Code::WrongArgumentCount,
        Code::NotImplemented,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::ExpressionSyntax => "expression_syntax",
            Code::LimitExceeded => "limit_exceeded",
            Code::UnknownContext => "unknown_context",
            Code::UnavailableContext => "unavailable_context",
            Code::UnknownFunction => "unknown_function",
            Code::UnavailableFunction => "unavailable_function",
            Code::WrongArgumentCount => "wrong_argument_count",
            Code::NotImplemented => "not_implemented",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: Code,
    pub message: String,
    pub span: Span,
}

impl Error {
    pub(crate) fn new(code: Code, span: Span, message: String) -> Error {
        Error {
            code,
            message,
            span,
        }
    }
}
