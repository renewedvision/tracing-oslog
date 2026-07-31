use crate::{
	bindings::{
		__dso_handle, _os_activity_create, _os_activity_current,
		os_activity_flag_t_OS_ACTIVITY_FLAG_DEFAULT, os_activity_scope_enter,
		os_activity_scope_leave, os_activity_scope_state_s, os_activity_scope_state_t,
		os_activity_t, os_log_create, os_log_t, os_log_type_t, os_log_type_t_OS_LOG_TYPE_DEBUG,
		os_log_type_t_OS_LOG_TYPE_DEFAULT, os_log_type_t_OS_LOG_TYPE_ERROR,
		os_log_type_t_OS_LOG_TYPE_FAULT, os_log_type_t_OS_LOG_TYPE_INFO, os_release,
		wrapped_os_log_default, wrapped_os_log_with_type, wrapped_os_log_with_type_private,
	},
	visitor::{AttributeMap, FieldVisitor},
};
use std::{
	collections::HashMap,
	ffi::CString,
	ops::Deref,
	ptr::addr_of_mut,
	sync::{
		atomic::{AtomicU64, Ordering},
		Mutex, OnceLock,
	},
};
use tracing_core::{
	span::{Attributes, Id},
	Event, Level, Subscriber,
};
use tracing_subscriber::{
	layer::{Context, Layer},
	registry::LookupSpan,
};

static NAMES: OnceLock<Mutex<HashMap<String, CString>>> = OnceLock::new();
static NEXT_CHUNKED_EVENT_ID: AtomicU64 = AtomicU64::new(1);

// Keep dynamic string data comfortably below unified logging's approximately 1 KiB per-entry
// payload limit. Chunk markers consume the remaining budget.
const OS_LOG_DYNAMIC_PAYLOAD_BUDGET: usize = 768;
const OS_LOG_CHUNK_CONTENT_BYTES: usize = 640;

struct Activity(os_activity_t);
// lol
unsafe impl Send for Activity {}
unsafe impl Sync for Activity {}
impl Deref for Activity {
	type Target = os_activity_t;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}
impl Drop for Activity {
	fn drop(&mut self) {
		unsafe {
			os_release(self.0 as *mut _);
		}
	}
}

pub struct OsLogger {
	logger: os_log_t,
}

impl Default for OsLogger {
	fn default() -> Self {
		Self {
			logger: unsafe { wrapped_os_log_default() },
		}
	}
}

impl OsLogger {
	/// Initialize a new `OsLogger`, which will output [tracing] events to
	/// os_log on Apple platforms.
	///
	/// # Arguments
	///
	/// * `subsystem` - An identifier string, in reverse DNS notation, that
	///   represents the subsystem that’s performing logging, for example,
	///   `com.your_company.your_subsystem_name`. The subsystem is used for
	///   categorization and filtering of related log messages, as well as for
	///   grouping related logging settings.
	/// * `category` - A category within the specified subsystem. The system
	///   uses the category to categorize and filter related log messages, as
	///   well as to group related logging settings within the subsystem’s
	///   settings. A category’s logging settings override those of the parent
	///   subsystem.
	pub fn new<S, C>(subsystem: S, category: C) -> Self
	where
		S: AsRef<str>,
		C: AsRef<str>,
	{
		let subsystem = CString::new(subsystem.as_ref())
			.expect("failed to construct C string from subsystem name");
		let category = CString::new(category.as_ref())
			.expect("failed to construct C string from category name");
		let logger = unsafe { os_log_create(subsystem.as_ptr(), category.as_ptr()) };
		Self { logger }
	}
}

unsafe impl Sync for OsLogger {}
unsafe impl Send for OsLogger {}

impl<S> Layer<S> for OsLogger
where
	S: Subscriber + for<'a> LookupSpan<'a>,
{
	fn on_new_span(&self, attrs: &Attributes, id: &Id, ctx: Context<S>) {
		let span = ctx.span(id).expect("invalid span, this shouldn't happen");
		let mut extensions = span.extensions_mut();
		if extensions.get_mut::<Activity>().is_none() {
			let mut names = NAMES.get_or_init(Mutex::default).lock().unwrap();
			let metadata = span.metadata();
			let parent_activity = match span.parent() {
				Some(parent) => **parent
					.extensions()
					.get::<Activity>()
					.expect("parent span didn't contain activity wtf"),
				None => addr_of_mut!(_os_activity_current),
			};
			let mut attributes = AttributeMap::default();
			let mut attr_visitor = FieldVisitor::new(&mut attributes);
			attrs.record(&mut attr_visitor);
			let name = {
				let function_name = [metadata.target(), metadata.name()].join("::");
				let full_name = format!(
					"{}({})",
					function_name,
					attributes
						.iter_public()
						.map(|(k, v)| format!("{}: {}", k, v))
						.collect::<Vec<_>>()
						.join(", ")
				);
				names.entry(full_name.clone()).or_insert_with(|| {
					CString::new(full_name).expect("failed to construct C string from span name")
				})
			};
			let activity = unsafe {
				_os_activity_create(
					addr_of_mut!(__dso_handle) as *mut _,
					name.as_ptr(),
					parent_activity,
					os_activity_flag_t_OS_ACTIVITY_FLAG_DEFAULT,
				)
			};
			extensions.insert(Activity(activity));
		}
	}

	fn on_event(&self, event: &Event, ctx: Context<S>) {
		let metadata = event.metadata();
		let level = tracing_level_to_oslog_level(*metadata.level());
		let mut attributes = AttributeMap::default();
		let mut attr_visitor = FieldVisitor::new(&mut attributes);
		event.record(&mut attr_visitor);

		let mut message = String::new();
		let mut message_is_private = false;
		if let Some((value, is_private)) = attributes.take_message() {
			message = value;
			message_is_private = is_private;
			if !is_private {
				message.push_str("  ");
			}
		}

		let (public_message, private_message) = if message_is_private {
			// The entire message is private — concatenate it with all remaining
			// attributes (private and public) so structured fields aren't lost.
			let attrs =
				format_attributes(attributes.iter_private().chain(attributes.iter_public()));
			if !attrs.is_empty() {
				if !message.is_empty() {
					message.push(' ');
				}
				message.push_str(&attrs);
			}
			// Public part is empty — wrapper.c will emit only the private portion.
			(String::new(), Some(message))
		} else {
			message.push_str(&format_attributes(attributes.iter_public()));
			let private = format_attributes(attributes.iter_private());
			let private = (!private.is_empty()).then_some(private);
			(message, private)
		};

		// Emit the log entry, optionally entering the current span's activity scope.
		let emit = || emit_message(self.logger, level, public_message, private_message);

		if let Some(parent_id) = ctx.current_span().id() {
			let span = ctx
				.span(parent_id)
				.expect("invalid span, this shouldn't happen");
			let mut extensions = span.extensions_mut();
			let activity = extensions
				.get_mut::<Activity>()
				.expect("span didn't contain activity wtf");

			let raw_state = [0u64; 2usize];
			unsafe {
				let state: os_activity_scope_state_s = std::mem::transmute(raw_state);
				let state: os_activity_scope_state_t = &state as *const _ as *mut _;
				os_activity_scope_enter(**activity, state);
				emit();
				os_activity_scope_leave(state);
			}
		} else {
			emit();
		}
	}

	fn on_enter(&self, _id: &Id, _ctx: Context<S>) {}

	fn on_exit(&self, _id: &Id, _ctx: Context<S>) {}

	fn on_close(&self, id: Id, ctx: Context<S>) {
		let span = ctx.span(&id).expect("invalid span, this shouldn't happen");
		let mut extensions = span.extensions_mut();
		extensions
			.remove::<Activity>()
			.expect("span didn't contain activity wtf");
	}
}

impl Drop for OsLogger {
	fn drop(&mut self) {
		unsafe {
			os_release(self.logger as *mut _);
		}
	}
}

/// Format an iterator of key-value attribute pairs as `k=v`, space-separated.
fn format_attributes<'a>(attrs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
	attrs
		.map(|(k, v)| format!("{}={}", k, v))
		.collect::<Vec<_>>()
		.join(" ")
}

fn emit_message(
	logger: os_log_t,
	level: os_log_type_t,
	public_message: String,
	private_message: Option<String>,
) {
	let event_id = NEXT_CHUNKED_EVENT_ID.fetch_add(1, Ordering::Relaxed);
	for message in plan_messages(public_message, private_message, event_id) {
		match message {
			PlannedMessage::Plain { public, private } => {
				let public = to_cstring(public);
				let private = private.map(to_cstring);
				unsafe {
					match private {
						Some(ref private) => wrapped_os_log_with_type_private(
							logger,
							level,
							public.as_ptr(),
							private.as_ptr(),
						),
						None => wrapped_os_log_with_type(logger, level, public.as_ptr()),
					}
				}
			}
			PlannedMessage::Chunk {
				marker,
				content,
				is_private,
			} => {
				if is_private {
					let marker = to_cstring(marker);
					let content = to_cstring(content);
					unsafe {
						wrapped_os_log_with_type_private(
							logger,
							level,
							marker.as_ptr(),
							content.as_ptr(),
						)
					};
				} else {
					let message = to_cstring(format!("{marker} {content}"));
					unsafe { wrapped_os_log_with_type(logger, level, message.as_ptr()) };
				}
			}
		}
	}
}

#[derive(Debug, PartialEq)]
enum PlannedMessage {
	Plain {
		public: String,
		private: Option<String>,
	},
	Chunk {
		marker: String,
		content: String,
		is_private: bool,
	},
}

fn plan_messages(
	mut public_message: String,
	mut private_message: Option<String>,
	event_id: u64,
) -> Vec<PlannedMessage> {
	strip_nuls(&mut public_message);
	if let Some(private) = &mut private_message {
		strip_nuls(private);
	}

	let dynamic_bytes =
		public_message.len() + private_message.as_ref().map_or(0, |private| private.len());
	if dynamic_bytes <= OS_LOG_DYNAMIC_PAYLOAD_BUDGET {
		return vec![PlannedMessage::Plain {
			public: public_message,
			private: private_message,
		}];
	}

	let mut planned = Vec::new();
	let public_chunks = utf8_chunks(&public_message, OS_LOG_CHUNK_CONTENT_BYTES);
	for (index, chunk) in public_chunks.iter().enumerate() {
		planned.push(PlannedMessage::Chunk {
			marker: format!(
				"[oslog chunk id={event_id} public {}/{}]",
				index + 1,
				public_chunks.len()
			),
			content: (*chunk).to_owned(),
			is_private: false,
		});
	}

	if let Some(private_message) = private_message {
		let private_chunks = utf8_chunks(&private_message, OS_LOG_CHUNK_CONTENT_BYTES);
		for (index, chunk) in private_chunks.iter().enumerate() {
			planned.push(PlannedMessage::Chunk {
				marker: format!(
					"[oslog chunk id={event_id} private {}/{}]",
					index + 1,
					private_chunks.len()
				),
				content: (*chunk).to_owned(),
				is_private: true,
			});
		}
	}

	planned
}

fn strip_nuls(value: &mut String) {
	value.retain(|character| character != '\0');
}

fn utf8_chunks(value: &str, maximum_bytes: usize) -> Vec<&str> {
	assert!(maximum_bytes >= 4, "chunk size must fit any UTF-8 scalar");
	if value.is_empty() {
		return Vec::new();
	}

	let mut chunks = Vec::new();
	let mut start = 0;
	let mut bytes = 0;
	for (index, character) in value.char_indices() {
		let character_bytes = character.len_utf8();
		if bytes != 0 && bytes + character_bytes > maximum_bytes {
			chunks.push(&value[start..index]);
			start = index;
			bytes = 0;
		}
		bytes += character_bytes;
	}
	chunks.push(&value[start..]);
	chunks
}

/// Convert a `String` into a `CString`, stripping interior NUL bytes that would
/// otherwise cause the conversion to fail.
fn to_cstring(mut s: String) -> CString {
	s.retain(|c| c != '\0');
	CString::new(s).expect("failed to convert message to a C string")
}

/// Convert `tracing::Level` to an os_log-compatible level.
///
/// Note that the semantics of these log levels don't match up 1-to-1, because
/// Apple's os_log is fairly old. See also
/// <https://github.com/Absolucy/tracing-oslog/issues/14>.
fn tracing_level_to_oslog_level(level: Level) -> os_log_type_t {
	match level {
		// Documented semantics:
		// > Use this level to capture information that may be useful
		// > during development or while troubleshooting a specific
		// > problem.
		//
		// This matches pretty well with `Level::TRACE`, especially since
		// `OS_LOG_TYPE_DEBUG` are only emitted through a configuration
		// change (similarly to how `Level::TRACE` is often disabled at
		// compile-time by default).
		Level::TRACE => os_log_type_t_OS_LOG_TYPE_DEBUG,
		// Documented semantics:
		// > Use this level to capture information that may be helpful,
		// > but not essential, for troubleshooting errors.
		//
		// I guess this could match semantically with either
		// `Level::DEBUG` or `Level::INFO`.
		Level::DEBUG => os_log_type_t_OS_LOG_TYPE_INFO,
		// Documented semantics:
		// > Use this level to capture information about things that might
		// > result in a failure.
		//
		// This is arguably slightly incorrect compared to the semantics
		// of `Level::INFO`, however we choose to do this since it's the
		// level that `NSLog` operates on.
		Level::INFO => os_log_type_t_OS_LOG_TYPE_DEFAULT,
		// Documented semantics:
		// > Use this log level to report process-level errors.
		//
		// However, we choose to veer from these, since in practice, users
		// expect warnings to show up as orange in Console.app / Xcode (which
		// this does).
		//
		// Note that this is also the leve that Swift's `Logger::warning`
		// emits at:
		// https://developer.apple.com/documentation/os/logger/warning(_:)
		Level::WARN => os_log_type_t_OS_LOG_TYPE_ERROR,
		// Documented semantics:
		// > Use this level only to capture system-level or multi-process
		// > information when reporting system errors.
		//
		// However, in practice many macOS applications use this logging level
		// for "weaker" errors that this.
		//
		// This logging level also makes the log entry red in Console.app /
		// Xcode, which is what a user would expect of a `Level::ERROR` entry.
		Level::ERROR => os_log_type_t_OS_LOG_TYPE_FAULT,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn utf8_chunks_reassemble_without_splitting_scalars() {
		let message = "alpha🙂beta🙂gamma";
		let chunks = utf8_chunks(message, 9);

		assert_eq!(chunks.concat(), message);
		assert!(chunks.iter().all(|chunk| chunk.len() <= 9));
	}

	#[test]
	fn utf8_chunks_keep_exact_boundary_in_one_chunk() {
		let message = "a".repeat(OS_LOG_CHUNK_CONTENT_BYTES);

		assert_eq!(
			utf8_chunks(&message, OS_LOG_CHUNK_CONTENT_BYTES),
			[message.as_str()]
		);
	}

	#[test]
	fn utf8_chunks_return_no_chunks_for_empty_message() {
		assert!(utf8_chunks("", OS_LOG_CHUNK_CONTENT_BYTES).is_empty());
	}

	#[test]
	fn planned_chunks_preserve_public_and_private_payloads() {
		let public = "public🙂".repeat(200);
		let private = "private🔒".repeat(200);
		let planned = plan_messages(public.clone(), Some(private.clone()), 42);

		let reconstructed_public = planned
			.iter()
			.filter_map(|message| match message {
				PlannedMessage::Chunk {
					content,
					is_private: false,
					..
				} => Some(content.as_str()),
				_ => None,
			})
			.collect::<String>();
		let reconstructed_private = planned
			.iter()
			.filter_map(|message| match message {
				PlannedMessage::Chunk {
					content,
					is_private: true,
					..
				} => Some(content.as_str()),
				_ => None,
			})
			.collect::<String>();

		assert_eq!(reconstructed_public, public);
		assert_eq!(reconstructed_private, private);
		assert!(planned.iter().all(|message| match message {
			PlannedMessage::Chunk {
				marker, content, ..
			} => marker.contains("id=42") && content.len() <= OS_LOG_CHUNK_CONTENT_BYTES,
			PlannedMessage::Plain { .. } => false,
		}));
	}

	#[test]
	fn short_message_keeps_public_and_private_fields_in_one_entry() {
		assert_eq!(
			plan_messages("public".to_owned(), Some("private".to_owned()), 42),
			[PlannedMessage::Plain {
				public: "public".to_owned(),
				private: Some("private".to_owned()),
			}]
		);
	}
}
