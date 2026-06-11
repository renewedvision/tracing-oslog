use std::{collections::BTreeMap, fmt::Debug};
use tracing_core::field::{Field, Visit};

use crate::private::PRIVATE_SENTINEL;

#[derive(Default)]
pub struct AttributeMap {
	public: BTreeMap<String, String>,
	private: BTreeMap<String, String>,
}

impl AttributeMap {
	/// Takes the message attribute, if available.
	///
	/// Returns `(value, is_private)`. When `is_private` is `true` the message
	/// was wrapped in [`Private`](crate::Private) at the call site and should
	/// be passed to OSLog's `%{private}s` formatter.
	pub fn take_message(&mut self) -> Option<(String, bool)> {
		if let Some(v) = self.public.remove("message") {
			Some((v, false))
		} else if let Some(v) = self.private.remove("message") {
			Some((v, true))
		} else {
			None
		}
	}

	/// Returns an iterator over public key-value pairs.
	pub fn iter_public(&self) -> impl Iterator<Item = (&str, &str)> {
		self.public.iter().map(|(k, v)| (k.as_str(), v.as_str()))
	}

	/// Returns an iterator over private key-value pairs.
	pub fn iter_private(&self) -> impl Iterator<Item = (&str, &str)> {
		self.private.iter().map(|(k, v)| (k.as_str(), v.as_str()))
	}
}

pub struct FieldVisitor<'a> {
	output: &'a mut AttributeMap,
}

impl<'a> FieldVisitor<'a> {
	pub fn new(output: &'a mut AttributeMap) -> Self {
		FieldVisitor { output }
	}
}

impl<'a> Visit for FieldVisitor<'a> {
	fn record_i64(&mut self, field: &Field, value: i64) {
		self.output
			.public
			.insert(field.name().to_string(), value.to_string());
	}

	fn record_u64(&mut self, field: &Field, value: u64) {
		self.output
			.public
			.insert(field.name().to_string(), value.to_string());
	}

	fn record_bool(&mut self, field: &Field, value: bool) {
		self.output
			.public
			.insert(field.name().to_string(), value.to_string());
	}

	fn record_str(&mut self, field: &Field, value: &str) {
		self.output
			.public
			.insert(field.name().to_string(), format!("\"{}\"", value));
	}

	fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
		let s = format!("{:?}", value);
		if let Some(inner) = s.strip_prefix(PRIVATE_SENTINEL) {
			self.output
				.private
				.insert(field.name().to_string(), inner.to_string());
		} else {
			self.output.public.insert(field.name().to_string(), s);
		}
	}
}
