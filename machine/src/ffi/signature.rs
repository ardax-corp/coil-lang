//! Explicit FFI signatures.

use crate::memory::FfiType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfiSignature {
    pub name: String,
    /// Fixed (non-variadic) argument types. When [`Self::variadic`] is set,
    /// this is the C prototype prefix before `...` (`nfixed = args.len()`).
    pub args: Vec<FfiType>,
    pub ret: FfiType,
    /// C-style varargs (`printf`-style). CIF is rebuilt per invoke.
    pub variadic: bool,
}

impl FfiSignature {
    /// Fixed-prefix arity (`nfixed` when variadic).
    pub fn arity(&self) -> usize {
        self.args.len()
    }

    pub fn from_parts(
        name: impl Into<String>,
        args: Vec<FfiType>,
        ret: FfiType,
    ) -> Result<Self, FfiError> {
        FfiSignatureBuilder::new(name).args(args).ret(ret).build()
    }
}

#[derive(Debug, Default)]
pub struct FfiSignatureBuilder {
    name: String,
    args: Vec<FfiType>,
    ret: Option<FfiType>,
    variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfiError {
    MissingName,
    MissingReturnType,
    VoidArgument {
        index: usize,
    },
    EmptyName,
    Libffi(String),
    ArityMismatch {
        expected: usize,
        got: usize,
    },
    SymbolNotFound {
        name: String,
    },
    LibraryNotFound {
        name: String,
        tried: Vec<String>,
        detail: String,
    },
    Unsupported(String),
    /// Bad library handle or out-of-range function id at invoke/declare time.
    InvalidHandle(String),
    /// Shared library blocked by the `dload` gate (stem list / extra allow+hash).
    LibraryDenied {
        name: String,
        stem: String,
        reason: String,
    },
    /// Process-exec symbol (`system`, `execve`, …) blocked unless `[env] allow_ffi_exec`.
    SymbolDenied {
        name: String,
    },
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingName => write!(f, "FFI signature requires a function name"),
            Self::MissingReturnType => write!(f, "FFI signature requires a return type"),
            Self::VoidArgument { index } => {
                write!(f, "FFI argument at index {index} cannot be void")
            }
            Self::EmptyName => write!(f, "FFI function name cannot be empty"),
            Self::Libffi(msg) => write!(f, "libffi error: {msg}"),
            Self::ArityMismatch { expected, got } => {
                write!(f, "FFI arity mismatch: expected {expected} args, got {got}")
            }
            Self::SymbolNotFound { name } => write!(f, "FFI symbol `{name}` not found in library"),
            Self::LibraryNotFound { name, tried, .. } => {
                write!(
                    f,
                    "FFI library `{name}` not found (tried: {})",
                    tried.join(", ")
                )
            }
            Self::Unsupported(msg) => write!(f, "unsupported FFI signature: {msg}"),
            Self::InvalidHandle(msg) => write!(f, "{msg}"),
            Self::LibraryDenied { name, stem, reason } => {
                write!(f, "FFI library `{name}` (stem `{stem}`) denied: {reason}")
            }
            Self::SymbolDenied { name } => {
                write!(
                    f,
                    "FFI symbol `{name}` denied: process exec requires [env] allow_ffi_exec"
                )
            }
        }
    }
}

impl std::error::Error for FfiError {}

impl FfiSignatureBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
            ret: None,
            variadic: false,
        }
    }

    pub fn arg(mut self, ty: FfiType) -> Self {
        self.args.push(ty);
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = FfiType>) -> Self {
        self.args.extend(args);
        self
    }

    pub fn ret(mut self, ty: FfiType) -> Self {
        self.ret = Some(ty);
        self
    }

    /// Mark as C-style varargs (`nfixed =` current arg count).
    pub fn variadic(mut self) -> Self {
        self.variadic = true;
        self
    }

    pub fn build(self) -> Result<FfiSignature, FfiError> {
        if self.name.is_empty() {
            return Err(FfiError::EmptyName);
        }
        for (index, ty) in self.args.iter().enumerate() {
            if ty.is_void() {
                return Err(FfiError::VoidArgument { index });
            }
        }
        let ret = self.ret.ok_or(FfiError::MissingReturnType)?;
        Ok(FfiSignature {
            name: self.name,
            args: self.args,
            ret,
            variadic: self.variadic,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_return_type() {
        let err = FfiSignatureBuilder::new("f")
            .arg(FfiType::Int)
            .build()
            .unwrap_err();
        assert_eq!(err, FfiError::MissingReturnType);
    }

    #[test]
    fn builder_variadic_flag() {
        let sig = FfiSignatureBuilder::new("printf")
            .arg(FfiType::String)
            .ret(FfiType::Int)
            .variadic()
            .build()
            .unwrap();
        assert!(sig.variadic);
        assert_eq!(sig.arity(), 1);
    }

    #[test]
    fn builder_rejects_void_arg() {
        let err = FfiSignatureBuilder::new("f")
            .arg(FfiType::Void)
            .ret(FfiType::Int)
            .build()
            .unwrap_err();
        assert_eq!(err, FfiError::VoidArgument { index: 0 });
    }
}
