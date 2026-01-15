//! Model string interning for memory-efficient model name storage.

use lasso::{MiniSpur, ThreadedRodeo};
use std::sync::LazyLock;

/// Global thread-safe string interner for model names.
/// Uses MiniSpur (2 bytes) instead of Spur (4 bytes) since we have < 65536 unique models.
static MODEL_INTERNER: LazyLock<ThreadedRodeo<MiniSpur>> = LazyLock::new(ThreadedRodeo::new);

/// Interned key for a model name.
///
/// Model names like "claude-3-5-sonnet" repeat across thousands of sessions.
/// Interning reduces memory from 24-byte String + heap per occurrence to 2-byte key.
///
/// Use [`intern_model`] to create a key and [`resolve_model`] to get the string back.
///
/// Note: `Default` is implemented to satisfy `TinyVec` bounds but should not be used
/// directly - the default value's resolution is undefined until the interner is populated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct ModelKey(MiniSpur);

impl ModelKey {
    /// Resolve this key back to its model name string.
    #[inline]
    pub fn resolve(self) -> &'static str {
        MODEL_INTERNER.resolve(&self.0)
    }
}

/// Intern a model name, returning a cheap 2-byte key.
#[inline]
pub fn intern_model(model: &str) -> ModelKey {
    ModelKey(MODEL_INTERNER.get_or_intern(model))
}

/// Resolve an interned model key back to its string.
#[inline]
pub fn resolve_model(key: ModelKey) -> &'static str {
    key.resolve()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_and_resolve() {
        let key = intern_model("claude-3-5-sonnet");
        assert_eq!(resolve_model(key), "claude-3-5-sonnet");
        assert_eq!(key.resolve(), "claude-3-5-sonnet");
    }

    #[test]
    fn same_string_same_key() {
        let key1 = intern_model("gpt-4");
        let key2 = intern_model("gpt-4");
        assert_eq!(key1, key2);
    }

    #[test]
    fn different_strings_different_keys() {
        let key1 = intern_model("model-a");
        let key2 = intern_model("model-b");
        assert_ne!(key1, key2);
    }
}
