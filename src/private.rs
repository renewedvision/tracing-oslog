/// Sentinel prefix embedded by `Private`'s `Debug` and `Display` impls.
/// The visitor strips this prefix to detect and route private field values.
pub(crate) const PRIVATE_SENTINEL: &str = "[[tracing_oslog::private]]";

/// Marks a field as private, causing it to be logged via oslog's
/// `%{private}s` mechanism. Private fields are redacted in Console.app by
/// default but can be unlocked by developers with a configuration profile or
/// `Info.plist` entry.
///
/// Use the `?` sigil in tracing macros to format via [`Debug`](std::fmt::Debug):
///
/// ```rust,ignore
/// #[derive(Debug)]
/// struct UserInfo { id: u64, email: String }
///
/// let user = UserInfo { id: 42, email: "alice@example.com".into() };
///
/// tracing::info!(
///     request_id = 99,                          // public
///     user = ?tracing_oslog::Private(&user),    // private — shows as <private>
/// );
/// ```
///
/// Or the `%` sigil for [`Display`](std::fmt::Display):
///
/// ```rust,ignore
/// tracing::info!(user = %tracing_oslog::Private(&user));
/// ```
pub struct Private<T>(pub T);

impl<T: std::fmt::Debug> std::fmt::Debug for Private<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{:?}", PRIVATE_SENTINEL, self.0)
	}
}

impl<T: std::fmt::Display> std::fmt::Display for Private<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{}", PRIVATE_SENTINEL, self.0)
	}
}
