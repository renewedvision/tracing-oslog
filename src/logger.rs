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
	callsite::Identifier,
	span::{Attributes, Id},
	Event, Level, Metadata, Subscriber,
};
use tracing_subscriber::{
	layer::{Context, Layer},
	registry::LookupSpan,
};

/// Activity names, one per span callsite. Keying by callsite bounds the cache
/// by the number of span sites in the program, however many spans are created
/// or whatever fields they carry.
static NAMES: OnceLock<Mutex<HashMap<Identifier, CString>>> = OnceLock::new();
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
	activities: bool,
}

impl Default for OsLogger {
	fn default() -> Self {
		Self {
			logger: unsafe { wrapped_os_log_default() },
			activities: true,
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
		Self {
			logger,
			activities: true,
		}
	}

	/// Choose whether spans are mapped to `os_activity` (the default).
	///
	/// Every span creates one activity, and each activity is one record in the
	/// unified log. Span-dense code (a CRDT library instrumenting every
	/// commit, say) can emit thousands of records per second, which evicts
	/// everything useful from the log's ring buffer. The activity's
	/// description is not recoverable from the log, so the records identify
	/// nothing.
	///
	/// With `false`, spans are ignored by this layer: no activity is created
	/// and events are emitted outside any activity (they still inherit
	/// whatever activity the calling thread is already in). Other layers on
	/// the same subscriber still see spans.
	pub fn with_activities(mut self, activities: bool) -> Self {
		self.activities = activities;
		self
	}
}

unsafe impl Sync for OsLogger {}
unsafe impl Send for OsLogger {}

impl<S> Layer<S> for OsLogger
where
	S: Subscriber + for<'a> LookupSpan<'a>,
{
	fn on_new_span(&self, attrs: &Attributes, id: &Id, ctx: Context<S>) {
		if !self.activities {
			return;
		}
		let Some(span) = ctx.span(id) else {
			return;
		};
		let mut extensions = span.extensions_mut();
		if extensions.get_mut::<Activity>().is_some() {
			return;
		}
		// The nearest ancestor this layer is tracking. Ancestors that a per-layer
		// filter hid from this layer have no activity, so they are skipped rather
		// than assumed.
		let parent_activity = span
			.scope()
			.skip(1)
			.find_map(|ancestor| ancestor.extensions().get::<Activity>().map(|a| **a))
			.unwrap_or(addr_of_mut!(_os_activity_current));
		let name = activity_name(attrs.metadata());
		let activity = unsafe {
			_os_activity_create(
				addr_of_mut!(__dso_handle) as *mut _,
				name,
				parent_activity,
				os_activity_flag_t_OS_ACTIVITY_FLAG_DEFAULT,
			)
		};
		extensions.insert(Activity(activity));
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

		// Enter the activity of the nearest enclosing span this layer is tracking, if
		// any. `event_span` honors explicit parents and per-layer filters; there may
		// be no such span, in which case the message is emitted outside any activity.
		let tracked = if self.activities {
			ctx.event_span(event).and_then(|span| {
				span.scope()
					.find(|candidate| candidate.extensions().get::<Activity>().is_some())
			})
		} else {
			None
		};
		let extensions = tracked.as_ref().map(|span| span.extensions());
		match extensions.as_ref().and_then(|e| e.get::<Activity>()) {
			Some(activity) => {
				let raw_state = [0u64; 2usize];
				unsafe {
					let state: os_activity_scope_state_s = std::mem::transmute(raw_state);
					let state: os_activity_scope_state_t = &state as *const _ as *mut _;
					os_activity_scope_enter(**activity, state);
					emit();
					os_activity_scope_leave(state);
				}
			}
			None => emit(),
		}
	}

	fn on_enter(&self, _id: &Id, _ctx: Context<S>) {}

	fn on_exit(&self, _id: &Id, _ctx: Context<S>) {}

	fn on_close(&self, id: Id, ctx: Context<S>) {
		if let Some(span) = ctx.span(&id) {
			span.extensions_mut().remove::<Activity>();
		}
	}
}

impl Drop for OsLogger {
	fn drop(&mut self) {
		unsafe {
			os_release(self.logger as *mut _);
		}
	}
}

/// The activity description for a span: its static `target::name`, interned
/// once per callsite so the pointer outlives every activity created from it.
///
/// Field values are deliberately not part of the name. They would make each
/// span's name unique, growing the cache without bound, and the log cannot
/// resolve a runtime description anyway.
fn activity_name(metadata: &'static Metadata<'static>) -> *const std::ffi::c_char {
	let mut names = NAMES
		.get_or_init(Mutex::default)
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	names
		.entry(metadata.callsite())
		.or_insert_with(|| {
			CString::new(
				[metadata.target(), metadata.name()]
					.join("::")
					.replace('\0', ""),
			)
			.expect("interior NULs were removed")
		})
		.as_ptr()
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

	use std::sync::{Arc, Mutex as StdMutex};
	use tracing_subscriber::{filter::filter_fn, layer::SubscriberExt, Layer};

	/// Records, for each span created, whether the `OsLogger` installed beneath
	/// it gave the span an activity. Layers added later run after earlier
	/// ones, so this sees their result.
	#[derive(Clone, Default)]
	struct ActivityProbe(Arc<StdMutex<Vec<(&'static str, bool)>>>);

	impl<S> Layer<S> for ActivityProbe
	where
		S: Subscriber + for<'a> LookupSpan<'a>,
	{
		fn on_new_span(&self, attrs: &Attributes, id: &Id, ctx: Context<S>) {
			let span = ctx.span(id).expect("span exists in the registry");
			let has_activity = span.extensions().get::<Activity>().is_some();
			self.0
				.lock()
				.unwrap()
				.push((attrs.metadata().name(), has_activity));
		}
	}

	fn probe_spans(
		logger: impl Layer<tracing_subscriber::Registry> + Send + Sync,
		run: impl FnOnce(),
	) -> Vec<(&'static str, bool)> {
		let probe = ActivityProbe::default();
		let subscriber = tracing_subscriber::registry()
			.with(logger)
			.with(probe.clone());
		tracing::subscriber::with_default(subscriber, run);
		let recorded = probe.0.lock().unwrap().clone();
		recorded
	}

	fn test_logger() -> OsLogger {
		OsLogger::new("com.renewedvision.tracing-oslog-tests", "tests")
	}

	#[test]
	fn spans_create_activities_by_default() {
		let spans = probe_spans(test_logger(), || {
			let outer = tracing::info_span!("default_outer");
			let _outer = outer.enter();
			let _inner = tracing::info_span!("default_inner").entered();
			tracing::info!("event inside nested activities");
		});
		assert_eq!(spans, [("default_outer", true), ("default_inner", true)]);
	}

	#[test]
	fn activities_off_creates_no_activity_and_still_logs_events() {
		let spans = probe_spans(test_logger().with_activities(false), || {
			let _outer = tracing::info_span!("off_outer").entered();
			let _inner = tracing::info_span!("off_inner").entered();
			tracing::info!("event inside spans this layer ignores");
		});
		assert_eq!(spans, [("off_outer", false), ("off_inner", false)]);
	}

	/// A child of a span this layer's filter hid attaches to the nearest
	/// tracked ancestor instead of assuming its direct parent has an activity.
	#[test]
	fn child_of_filtered_span_does_not_panic_and_is_tracked() {
		let logger = test_logger().with_filter(filter_fn(|metadata| metadata.name() != "hidden"));
		let spans = probe_spans(logger, || {
			let _outer = tracing::info_span!("visible_outer").entered();
			let _hidden = tracing::info_span!("hidden").entered();
			let _inner = tracing::info_span!("visible_inner").entered();
			tracing::info!("event under a hidden parent");
		});
		assert_eq!(spans, [
			("visible_outer", true),
			("hidden", false),
			("visible_inner", true)
		]);
	}

	/// Spans from one callsite share one interned name however many distinct
	/// field values they carry, so the cache is bounded by the number of span
	/// sites.
	#[test]
	fn unique_field_values_share_one_cached_name() {
		let before = cached_names_containing("ppc550_unique_fields");
		probe_spans(test_logger(), || {
			for id in 0..1_000u64 {
				let _span = tracing::info_span!("ppc550_unique_fields", id).entered();
			}
		});
		assert_eq!(cached_names_containing("ppc550_unique_fields") - before, 1);
	}

	fn cached_names_containing(needle: &str) -> usize {
		NAMES
			.get()
			.map(|names| {
				names
					.lock()
					.unwrap()
					.values()
					.filter(|name| name.to_string_lossy().contains(needle))
					.count()
			})
			.unwrap_or(0)
	}

	/// An event inside a span that this layer's own filter rejected has no
	/// activity to enter, and must be emitted unscoped instead of panicking.
	#[test]
	fn event_inside_span_filtered_from_this_layer_does_not_panic() {
		// Without a layer that enables spans, the registry never creates this one, and
		// the path under test is not reached.
		struct EnableEverything;
		impl<S: Subscriber> Layer<S> for EnableEverything {}

		let subscriber = tracing_subscriber::registry().with(EnableEverything).with(
			OsLogger::new("com.renewedvision.tracing-oslog-tests", "filtered")
				.with_filter(filter_fn(|metadata| !metadata.is_span())),
		);
		tracing::subscriber::with_default(subscriber, || {
			let span = tracing::info_span!("filtered_span");
			let _entered = span.enter();
			tracing::info!("event inside a span the oslog layer never saw");
		});
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
