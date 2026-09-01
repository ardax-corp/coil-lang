//! Stable diagnostic error codes for coil.

/// Stable machine-readable diagnostic code.
///
/// Codes are grouped by family:
/// - `E00xx` — parse / syntax
/// - `E01xx` — name resolution & types
/// - `E02xx` — enums / match / constructs
/// - `E03xx` — format strings & builtins
/// - `E04xx` — aggregates / records / FFI tags
/// - `E08xx` — codegen
/// - `E09xx` — CLI / I/O / archive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // --- Parse (E00xx) ---
    ParseError,

    // --- Name resolution & types (E01xx) ---
    UnknownValue,
    UnknownFunction,
    TypeMismatch,
    InfiniteType,
    NotAFunction,
    TooManyArguments,
    UndeclaredAssignment,
    InvalidAssignment,
    VariableRedeclaration,
    ConstantRedeclaration,
    UnknownType,
    ReturnMismatch,
    YieldOutsideAsync,
    ResumeTypeMismatch,
    InvalidTry,
    InvalidCoalesce,
    InvalidOptionalAccess,
    ConflictingErrorType,
    GenericTypeError,
    /// No overload of a function accepts the given argument count.
    WrongArity,
    /// Two overloads of the same function have conflicting arities.
    DuplicateOverload,
    /// A function name used in value position is ambiguous among overloads.
    AmbiguousOverload,

    /// `use path::*` — wildcards are banned; prelude is auto-imported instead.
    WildcardImport,

    /// Code after a diverging statement (`return` / `raise` / `panic` / infinite loop).
    UnreachableCode,
    /// `defer` that cannot run on function exit (dominated by / inside infinite loop).
    DeferNeverRuns,
    /// A single expression is nested deep enough to risk overflowing the
    /// compiler's own native stack (typecheck or codegen recursion) — not to
    /// be confused with [`Self::UnboundedRecursion`], which is about the
    /// *compiled program's* runtime call stack.
    ExpressionNestingTooDeep,
    /// Invalid `fn drop(self)` (wrong owner, arity, static, duplicate, …).
    InvalidDrop,
    /// Free generic `fn f<T>(...) -> Option<U>` where `U` mentions a type
    /// parameter. A shared generic body boxes `T`; wrapping that box in
    /// Option corrupts native payloads. Inherent methods are monomorphized
    /// and stay valid.
    UnsupportedGenericOptionReturn,
    /// Private field or inherent method used outside its type's `impl`.
    PrivateMember,

    // --- Enums / match / constructs (E02xx) ---
    DuplicateEnum,
    DuplicateConstructor,
    UnknownEnum,
    UnknownVariant,
    ConstructorArity,
    PayloadShapeMismatch,
    MissingField,
    UnknownField,
    DuplicateField,
    NonExhaustiveMatch,
    UnreachableArm,
    UnknownConstructorPattern,
    AmbiguousField,

    // --- Format / print (E03xx) ---
    FormatSpecifierMismatch,
    FormatArityMismatch,

    // --- Aggregates / FFI (E04xx) ---
    IndexOutOfBounds,
    CannotIndex,
    ArrayElementMismatch,
    InvalidFfiType,
    DeclareArity,
    InvokeArity,
    /// `env::exec` without `--allow-exec`.
    HostExecDenied,
    /// `env::exit` without `--allow-exit`.
    HostExitDenied,
    /// `Stream.attach` without `--allow-attach`.
    HostAttachDenied,
    /// FFI process-exec symbol without `--allow-ffi-exec`.
    HostFfiExecDenied,
    /// `dload` of a const stem that is not granted (or a libc alias).
    HostDloadDenied,
    /// `dload` path is not a compile-time string (would leak the runtime allowlist).
    HostDloadNonConst,

    // --- Codegen (E08xx) ---
    UnknownExpression,
    CodegenError,
    /// Recursive function depth cannot be proven; `#[max_depth(N)]` required.
    UnboundedRecursion,
    /// Proven / attributed recursion depth exceeds the VM operand-stack capacity.
    StackDepthExceeded,
    /// Monomorphization per-fn or total cap was hit; extra specs were not emitted.
    MonomorphizeCap,

    // --- CLI / I/O (E09xx) ---
    IoError,
    ArchiveVersionMismatch,
    InvalidCliFlags,
    MissingInputFile,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "E0001",
            Self::UnknownValue => "E0100",
            Self::UnknownFunction => "E0101",
            Self::TypeMismatch => "E0102",
            Self::InfiniteType => "E0103",
            Self::NotAFunction => "E0104",
            Self::TooManyArguments => "E0105",
            Self::UndeclaredAssignment => "E0106",
            Self::InvalidAssignment => "E0107",
            Self::VariableRedeclaration => "E0108",
            Self::ConstantRedeclaration => "E0109",
            Self::UnknownType => "E0110",
            Self::ReturnMismatch => "E0111",
            Self::YieldOutsideAsync => "E0112",
            Self::ResumeTypeMismatch => "E0113",
            Self::InvalidTry => "E0114",
            Self::InvalidCoalesce => "E0115",
            Self::InvalidOptionalAccess => "E0116",
            Self::ConflictingErrorType => "E0117",
            Self::GenericTypeError => "E0119",
            Self::WrongArity => "E0120",
            Self::DuplicateOverload => "E0121",
            Self::AmbiguousOverload => "E0122",
            Self::WildcardImport => "E0124",
            Self::UnreachableCode => "E0118",
            Self::DeferNeverRuns => "E0123",
            Self::ExpressionNestingTooDeep => "E0125",
            Self::InvalidDrop => "E0126",
            Self::UnsupportedGenericOptionReturn => "E0127",
            Self::PrivateMember => "E0128",
            Self::DuplicateEnum => "E0200",
            Self::DuplicateConstructor => "E0201",
            Self::UnknownEnum => "E0202",
            Self::UnknownVariant => "E0203",
            Self::ConstructorArity => "E0204",
            Self::PayloadShapeMismatch => "E0205",
            Self::MissingField => "E0206",
            Self::UnknownField => "E0207",
            Self::DuplicateField => "E0208",
            Self::NonExhaustiveMatch => "E0209",
            Self::UnreachableArm => "E0210",
            Self::UnknownConstructorPattern => "E0211",
            Self::AmbiguousField => "E0212",
            Self::FormatSpecifierMismatch => "E0300",
            Self::FormatArityMismatch => "E0301",
            Self::IndexOutOfBounds => "E0400",
            Self::CannotIndex => "E0401",
            Self::ArrayElementMismatch => "E0402",
            Self::InvalidFfiType => "E0403",
            Self::DeclareArity => "E0404",
            Self::InvokeArity => "E0405",
            Self::HostExecDenied => "E0406",
            Self::HostExitDenied => "E0407",
            Self::HostAttachDenied => "E0408",
            Self::HostFfiExecDenied => "E0409",
            Self::HostDloadDenied => "E0410",
            Self::HostDloadNonConst => "E0411",
            Self::UnknownExpression => "E0800",
            Self::CodegenError => "E0801",
            Self::UnboundedRecursion => "E0802",
            Self::StackDepthExceeded => "E0803",
            Self::MonomorphizeCap => "E0804",
            Self::IoError => "E0900",
            Self::ArchiveVersionMismatch => "E0901",
            Self::InvalidCliFlags => "E0902",
            Self::MissingInputFile => "E0903",
        }
    }

    /// Numeric form for LSP `Diagnostic.code` (number variant).
    pub fn as_number(self) -> i32 {
        let s = self.as_str();
        s.trim_start_matches('E').parse().unwrap_or(0)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ParseError => "parse error",
            Self::UnknownValue => "cannot find value in this scope",
            Self::UnknownFunction => "cannot find function",
            Self::TypeMismatch => "type mismatch",
            Self::InfiniteType => "infinite type",
            Self::NotAFunction => "not a function",
            Self::TooManyArguments => "too many arguments",
            Self::UndeclaredAssignment => "assignment to undeclared variable",
            Self::InvalidAssignment => "invalid assignment target",
            Self::VariableRedeclaration => "variable redeclaration",
            Self::ConstantRedeclaration => "constant redeclaration",
            Self::UnknownType => "unknown type",
            Self::ReturnMismatch => "return type mismatch",
            Self::YieldOutsideAsync => "yield outside async fn",
            Self::ResumeTypeMismatch => "resume type mismatch",
            Self::InvalidTry => "invalid try operator",
            Self::InvalidCoalesce => "invalid coalesce operator",
            Self::InvalidOptionalAccess => "invalid optional access",
            Self::ConflictingErrorType => "conflicting error types",
            Self::GenericTypeError => "type error",
            Self::WrongArity => "no matching overload for argument count",
            Self::DuplicateOverload => "duplicate overload: conflicting arities",
            Self::AmbiguousOverload => "ambiguous overload in value position",
            Self::WildcardImport => "wildcard import is not allowed",
            Self::UnreachableCode => "unreachable code",
            Self::DeferNeverRuns => "defer will never run on function exit",
            Self::ExpressionNestingTooDeep => "expression nested too deeply for the compiler",
            Self::InvalidDrop => "invalid drop method",
            Self::UnsupportedGenericOptionReturn => "unsupported free generic Option return",
            Self::PrivateMember => "private member is not accessible",
            Self::DuplicateEnum => "duplicate enum",
            Self::DuplicateConstructor => "duplicate constructor",
            Self::UnknownEnum => "unknown enum",
            Self::UnknownVariant => "unknown variant",
            Self::ConstructorArity => "constructor arity mismatch",
            Self::PayloadShapeMismatch => "payload shape mismatch",
            Self::MissingField => "missing field",
            Self::UnknownField => "unknown field",
            Self::DuplicateField => "duplicate field",
            Self::NonExhaustiveMatch => "non-exhaustive match",
            Self::UnreachableArm => "unreachable match arm",
            Self::UnknownConstructorPattern => "unknown constructor in pattern",
            Self::AmbiguousField => "ambiguous field access",
            Self::FormatSpecifierMismatch => "format specifier type mismatch",
            Self::FormatArityMismatch => "format argument count mismatch",
            Self::IndexOutOfBounds => "index out of bounds",
            Self::CannotIndex => "cannot index non-aggregate",
            Self::ArrayElementMismatch => "array element type mismatch",
            Self::InvalidFfiType => "invalid FFI type",
            Self::DeclareArity => "declare argument mismatch",
            Self::InvokeArity => "invoke argument mismatch",
            Self::HostExecDenied => "env::exec is not granted",
            Self::HostExitDenied => "env::exit is not granted",
            Self::HostAttachDenied => "Stream.attach is not granted",
            Self::HostFfiExecDenied => "FFI process-exec is not granted",
            Self::HostDloadDenied => "dload stem is not granted",
            Self::HostDloadNonConst => "dload path must be a string literal",
            Self::UnknownExpression => "unknown expression in codegen",
            Self::CodegenError => "codegen error",
            Self::UnboundedRecursion => "unbounded recursion depth",
            Self::StackDepthExceeded => "stack depth exceeds VM limit",
            Self::MonomorphizeCap => "monomorphization specialization cap hit",
            Self::IoError => "I/O error",
            Self::ArchiveVersionMismatch => "bytecode archive version mismatch",
            Self::InvalidCliFlags => "invalid CLI flags",
            Self::MissingInputFile => "missing input file",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive list — adding a variant without updating this match fails to compile.
    fn all_error_codes() -> Vec<ErrorCode> {
        use ErrorCode::*;
        let sample = ParseError;
        match sample {
            ParseError
            | UnknownValue
            | UnknownFunction
            | TypeMismatch
            | InfiniteType
            | NotAFunction
            | TooManyArguments
            | UndeclaredAssignment
            | InvalidAssignment
            | VariableRedeclaration
            | ConstantRedeclaration
            | UnknownType
            | ReturnMismatch
            | YieldOutsideAsync
            | ResumeTypeMismatch
            | InvalidTry
            | InvalidCoalesce
            | InvalidOptionalAccess
            | ConflictingErrorType
            | GenericTypeError
            | WrongArity
            | DuplicateOverload
            | AmbiguousOverload
            | WildcardImport
            | UnreachableCode
            | DeferNeverRuns
            | ExpressionNestingTooDeep
            | InvalidDrop
            | UnsupportedGenericOptionReturn
            | PrivateMember
            | DuplicateEnum
            | DuplicateConstructor
            | UnknownEnum
            | UnknownVariant
            | ConstructorArity
            | PayloadShapeMismatch
            | MissingField
            | UnknownField
            | DuplicateField
            | NonExhaustiveMatch
            | UnreachableArm
            | UnknownConstructorPattern
            | AmbiguousField
            | FormatSpecifierMismatch
            | FormatArityMismatch
            | IndexOutOfBounds
            | CannotIndex
            | ArrayElementMismatch
            | InvalidFfiType
            | DeclareArity
            | InvokeArity
            | HostExecDenied
            | HostExitDenied
            | HostAttachDenied
            | HostFfiExecDenied
            | HostDloadDenied
            | HostDloadNonConst
            | UnknownExpression
            | CodegenError
            | UnboundedRecursion
            | StackDepthExceeded
            | MonomorphizeCap
            | IoError
            | ArchiveVersionMismatch
            | InvalidCliFlags
            | MissingInputFile => {}
        }
        vec![
            ParseError,
            UnknownValue,
            UnknownFunction,
            TypeMismatch,
            InfiniteType,
            NotAFunction,
            TooManyArguments,
            UndeclaredAssignment,
            InvalidAssignment,
            VariableRedeclaration,
            ConstantRedeclaration,
            UnknownType,
            ReturnMismatch,
            YieldOutsideAsync,
            ResumeTypeMismatch,
            InvalidTry,
            InvalidCoalesce,
            InvalidOptionalAccess,
            ConflictingErrorType,
            GenericTypeError,
            WrongArity,
            DuplicateOverload,
            AmbiguousOverload,
            UnreachableCode,
            DeferNeverRuns,
            ExpressionNestingTooDeep,
            InvalidDrop,
            UnsupportedGenericOptionReturn,
            PrivateMember,
            DuplicateEnum,
            DuplicateConstructor,
            UnknownEnum,
            UnknownVariant,
            ConstructorArity,
            PayloadShapeMismatch,
            MissingField,
            UnknownField,
            DuplicateField,
            NonExhaustiveMatch,
            UnreachableArm,
            UnknownConstructorPattern,
            AmbiguousField,
            FormatSpecifierMismatch,
            FormatArityMismatch,
            IndexOutOfBounds,
            CannotIndex,
            ArrayElementMismatch,
            InvalidFfiType,
            DeclareArity,
            InvokeArity,
            HostExecDenied,
            HostExitDenied,
            HostAttachDenied,
            HostFfiExecDenied,
            HostDloadDenied,
            HostDloadNonConst,
            UnknownExpression,
            CodegenError,
            UnboundedRecursion,
            StackDepthExceeded,
            MonomorphizeCap,
            IoError,
            ArchiveVersionMismatch,
            InvalidCliFlags,
            MissingInputFile,
        ]
    }

    #[test]
    fn codes_are_unique_strings() {
        let all = all_error_codes();
        let mut seen = std::collections::HashSet::new();
        for c in &all {
            assert!(
                seen.insert(c.as_str()),
                "duplicate code string {}",
                c.as_str()
            );
            assert!(c.as_number() > 0, "{} should parse as positive", c.as_str());
            assert_eq!(c.to_string(), c.as_str());
            assert!(!c.description().is_empty());
        }
        assert_eq!(all.len(), seen.len());
    }

    #[test]
    fn display_matches_as_str_for_every_code() {
        for c in all_error_codes() {
            assert_eq!(format!("{c}"), c.as_str());
        }
    }
}
