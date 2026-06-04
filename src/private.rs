use valuable::{Fields, StructDef, Structable, Valuable, Value};

pub(crate) const PRIVATE_SENTINEL: &str = "tracing_oslog::Private";

static EMPTY_FIELDS: &[valuable::NamedField<'static>] = &[];

/// Marks a [`Valuable`] field as private, causing it to be logged via oslog's
/// `%{private}s` mechanism. Private fields are redacted in Console.app by
/// default but can be unlocked by developers with a configuration profile or
/// `Info.plist` entry.
///
/// Requires the `valuable` crate feature and `RUSTFLAGS="--cfg tracing_unstable"`.
///
/// # Example
///
/// ```rust,ignore
/// use valuable::Valuable;
///
/// #[derive(Debug, Valuable)]
/// struct UserInfo { id: u64, email: String }
///
/// let user = UserInfo { id: 42, email: "alice@example.com".into() };
///
/// tracing::info!(
///     request_id = 99,                        // public
///     user = tracing_oslog::Private(&user),   // private — shows as <private>
/// );
/// ```
pub struct Private<T: Valuable>(pub T);

impl<T: Valuable> Valuable for Private<T> {
	fn as_value(&self) -> Value<'_> {
		Value::Structable(self)
	}

	fn visit(&self, visit: &mut dyn valuable::Visit) {
		// Forward the inner value so our InnerCapture visitor can format it.
		visit.visit_value(self.0.as_value())
	}
}

impl<T: Valuable> Structable for Private<T> {
	fn definition(&self) -> StructDef<'_> {
		// Provides a placeholder while the inner value is formatted using the visitor.
		StructDef::new_static(PRIVATE_SENTINEL, Fields::Named(EMPTY_FIELDS))
	}
}

/// Allows `Private<T>` to be used directly as a tracing field value.
///
/// Requires `tracing_unstable` because `Visit::record_value` is only
/// available under that cfg.
#[cfg(tracing_unstable)]
impl<T: Valuable> tracing_core::field::Value for Private<T> {
	fn record(
		&self,
		key: &tracing_core::field::Field,
		visitor: &mut dyn tracing_core::field::Visit,
	) {
		visitor.record_value(key, self.as_value())
	}
}
