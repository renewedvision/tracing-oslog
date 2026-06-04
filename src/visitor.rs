use std::{collections::BTreeMap, fmt::Debug};
use tracing_core::field::{Field, Visit};

use crate::private::PRIVATE_SENTINEL;

#[derive(Default)]
pub struct AttributeMap {
	pub public: BTreeMap<String, String>,
	pub private: BTreeMap<String, String>,
}

impl AttributeMap {
	/// Takes the message attribute, if available.
	pub fn take_message(&mut self) -> Option<String> {
		self.public.remove("message")
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
