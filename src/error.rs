//! `ReceiptsError`: the one error type for the whole crate, carrying enough
//! context (provider, retryability, partial pipeline output) to populate the
//! `receipts.cli.error.v1` envelope (see `envelope.rs`) without re-deriving it.

use serde_json::Value;

/// Which upstream API an error originated from, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Cerebras,
    Exa,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Cerebras => "cerebras",
            Provider::Exa => "exa",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Public only because enum struct-variant fields inherit the enum's own
// visibility — this type is not meant to be constructed outside this module.
#[derive(Debug, Clone, Default)]
pub struct ErrorContext {
    provider: Option<Provider>,
    retryable: bool,
    partial: Option<Value>,
    suggested_fix: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptsError {
    #[error("usage error: {message}")]
    Usage {
        message: String,
        context: ErrorContext,
    },

    #[error("authentication failed: {message}")]
    Auth {
        message: String,
        context: ErrorContext,
    },

    #[error("configuration error: {message}")]
    Config {
        message: String,
        context: ErrorContext,
    },

    #[error("network error: {message}")]
    Network {
        message: String,
        context: ErrorContext,
    },

    #[error("upstream error: {message}")]
    Upstream {
        message: String,
        context: ErrorContext,
    },

    #[error("rate limited: {message}")]
    RateLimit {
        message: String,
        context: ErrorContext,
    },

    /// Partial result from a hard pipeline failure. Budget-driven partials
    /// (exit 10) must use the SUCCESS envelope path in `commands::ask` (exit
    /// 10 via the success envelope, not via this error variant). This variant
    /// exists for hard failures where an error envelope carries `error.partial`
    /// data alongside the failure context.
    #[error("partial result: {message}")]
    Partial {
        message: String,
        context: ErrorContext,
    },

    #[error("no input: {message}")]
    NoInput {
        message: String,
        context: ErrorContext,
    },
}

macro_rules! constructor {
    ($name:ident, $variant:ident) => {
        pub fn $name(message: impl Into<String>) -> Self {
            ReceiptsError::$variant {
                message: message.into(),
                context: ErrorContext::default(),
            }
        }
    };
}

impl ReceiptsError {
    constructor!(usage, Usage);
    constructor!(auth, Auth);
    constructor!(config, Config);
    constructor!(network, Network);
    constructor!(upstream, Upstream);
    constructor!(rate_limit, RateLimit);
    constructor!(partial, Partial);
    constructor!(no_input, NoInput);

    fn context(&self) -> &ErrorContext {
        match self {
            ReceiptsError::Usage { context, .. }
            | ReceiptsError::Auth { context, .. }
            | ReceiptsError::Config { context, .. }
            | ReceiptsError::Network { context, .. }
            | ReceiptsError::Upstream { context, .. }
            | ReceiptsError::RateLimit { context, .. }
            | ReceiptsError::Partial { context, .. }
            | ReceiptsError::NoInput { context, .. } => context,
        }
    }

    fn context_mut(&mut self) -> &mut ErrorContext {
        match self {
            ReceiptsError::Usage { context, .. }
            | ReceiptsError::Auth { context, .. }
            | ReceiptsError::Config { context, .. }
            | ReceiptsError::Network { context, .. }
            | ReceiptsError::Upstream { context, .. }
            | ReceiptsError::RateLimit { context, .. }
            | ReceiptsError::Partial { context, .. }
            | ReceiptsError::NoInput { context, .. } => context,
        }
    }

    #[must_use]
    pub fn with_provider(mut self, provider: Provider) -> Self {
        self.context_mut().provider = Some(provider);
        self
    }

    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.context_mut().retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_partial(mut self, partial: Value) -> Self {
        self.context_mut().partial = Some(partial);
        self
    }

    #[must_use]
    pub fn with_suggested_fix(mut self, suggested_fix: impl Into<String>) -> Self {
        self.context_mut().suggested_fix = Some(suggested_fix.into());
        self
    }

    pub fn provider(&self) -> Option<Provider> {
        self.context().provider
    }

    pub fn is_retryable(&self) -> bool {
        self.context().retryable
    }

    pub fn partial_data(&self) -> Option<&Value> {
        self.context().partial.as_ref()
    }

    pub fn suggested_fix(&self) -> Option<&str> {
        self.context().suggested_fix.as_deref()
    }

    /// Stable exit code for the process, per the CLI contract.
    pub fn exit_code(&self) -> i32 {
        match self {
            ReceiptsError::Usage { .. } => 1,
            ReceiptsError::Auth { .. } => 2,
            ReceiptsError::Config { .. } => 3,
            ReceiptsError::Network { .. } => 4,
            ReceiptsError::Upstream { .. } => 5,
            ReceiptsError::RateLimit { .. } => 6,
            ReceiptsError::Partial { .. } => 10,
            ReceiptsError::NoInput { .. } => 11,
        }
    }

    /// Stable snake_case identifier for the error envelope's `error.code`.
    pub fn code(&self) -> &'static str {
        match self {
            ReceiptsError::Usage { .. } => "usage",
            ReceiptsError::Auth { .. } => "auth",
            ReceiptsError::Config { .. } => "config",
            ReceiptsError::Network { .. } => "network",
            ReceiptsError::Upstream { .. } => "upstream",
            ReceiptsError::RateLimit { .. } => "rate_limited",
            ReceiptsError::Partial { .. } => "partial",
            ReceiptsError::NoInput { .. } => "no_input",
        }
    }

    /// Coarse category for the error envelope's `error.category`; currently
    /// mirrors `code()` one-to-one because the contract exposes both fields.
    pub fn category(&self) -> &'static str {
        self.code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(ReceiptsError::usage("x").exit_code(), 1);
        assert_eq!(ReceiptsError::auth("x").exit_code(), 2);
        assert_eq!(ReceiptsError::config("x").exit_code(), 3);
        assert_eq!(ReceiptsError::network("x").exit_code(), 4);
        assert_eq!(ReceiptsError::upstream("x").exit_code(), 5);
        assert_eq!(ReceiptsError::rate_limit("x").exit_code(), 6);
        assert_eq!(ReceiptsError::partial("x").exit_code(), 10);
        assert_eq!(ReceiptsError::no_input("x").exit_code(), 11);
    }

    #[test]
    fn codes_are_stable_snake_case() {
        assert_eq!(ReceiptsError::usage("x").code(), "usage");
        assert_eq!(ReceiptsError::auth("x").code(), "auth");
        assert_eq!(ReceiptsError::rate_limit("x").code(), "rate_limited");
        assert_eq!(ReceiptsError::no_input("x").code(), "no_input");
    }

    #[test]
    fn builders_set_context() {
        let err = ReceiptsError::network("timed out")
            .with_provider(Provider::Cerebras)
            .with_retryable(true)
            .with_partial(serde_json::json!({"claims": []}));

        assert_eq!(err.provider(), Some(Provider::Cerebras));
        assert!(err.is_retryable());
        assert_eq!(err.partial_data(), Some(&serde_json::json!({"claims": []})));
        assert_eq!(err.to_string(), "network error: timed out");
    }

    #[test]
    fn suggested_fix_builder_sets_context() {
        let err = ReceiptsError::auth("missing Cerebras API key")
            .with_provider(Provider::Cerebras)
            .with_suggested_fix("set CEREBRAS_API_KEY");

        assert_eq!(err.suggested_fix(), Some("set CEREBRAS_API_KEY"));
    }

    #[test]
    fn default_context_has_no_provider_and_not_retryable() {
        let err = ReceiptsError::usage("bad flag");
        assert_eq!(err.provider(), None);
        assert!(!err.is_retryable());
        assert_eq!(err.partial_data(), None);
        assert_eq!(err.suggested_fix(), None);
    }
}
