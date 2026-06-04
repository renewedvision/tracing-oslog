use std::{collections::BTreeMap, fmt::Debug};
use tracing_core::field::{Field, Visit};

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
		self.output
			.public
			.insert(field.name().to_string(), format!("{:?}", value));
	}

	#[cfg(all(tracing_unstable, feature = "valuable"))]
	fn record_value(&mut self, field: &Field, value: valuable::Value<'_>) {
		use crate::private::PRIVATE_SENTINEL;

		let is_private = value
			.as_structable()
			.map(|s| s.definition().name() == PRIVATE_SENTINEL)
			.unwrap_or(false);

		if is_private {
			// Capture the inner value by forwarding through Private::visit(),
			// which calls visit_value(self.0.as_value()).
			struct InnerCapture(Option<String>);
			impl valuable::Visit for InnerCapture {
				fn visit_value(&mut self, value: valuable::Value<'_>) {
					self.0 = Some(format!("{:?}", value));
				}
			}
			let mut capture = InnerCapture(None);
			if let Some(s) = value.as_structable() {
				s.visit(&mut capture);
			}
			self.output
				.private
				.insert(field.name().to_string(), capture.0.unwrap_or_default());
		} else {
			self.output
				.public
				.insert(field.name().to_string(), format!("{:?}", value));
		}
	}
}
