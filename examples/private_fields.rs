//! Demonstrates logging with private fields using the `valuable` feature.
//!
//! Private fields are redacted in Console.app by default (shown as `<private>`),
//! but can be unlocked for debugging by adding an `OSLogPreferences` entry to
//! your app's `Info.plist`:
//!
//! ```xml
//! <key>OSLogPreferences</key>
//! <dict>
//!     <key>com.example.myapp</key>
//!     <dict>
//!         <key>auth</key>
//!         <dict>
//!             <key>Enable-Private-Data</key>
//!             <true/>
//!         </dict>
//!     </dict>
//! </dict>
//! ```
//!
//! Run with:
//!
//! ```sh
//! RUSTFLAGS="--cfg tracing_unstable" cargo run --example private_fields --features valuable
//! ```

#[cfg(tracing_unstable)]
fn main() {
	use tracing_subscriber::prelude::*;
	use valuable::Valuable;

	#[derive(Debug, Valuable)]
	struct UserInfo {
		id: u64,
		email: String,
	}

	let logger = tracing_oslog::OsLogger::new("com.example.myapp", "auth");
	tracing_subscriber::registry().with(logger).init();

	let user = UserInfo {
		id: 42,
		email: "alice@example.com".to_string(),
	};

	// `request_id` and `endpoint` are public, so they're visible in the logs.
	// `user` is wrapped in `Private`, so it will show as `<private>` in the
	// logs unless private data is enabled for this subsystem/category.
	tracing::info!(
		request_id = 99,
		endpoint = "/api/login",
		user = tracing_oslog::Private(&user),
	);
}

#[cfg(not(tracing_unstable))]
fn main() {
	eprintln!(
		"This example requires the `tracing_unstable` cfg flag.\n\
		 Re-run with:\n\
		 RUSTFLAGS=\"--cfg tracing_unstable\" cargo run --example private_fields --features valuable"
	);
}
