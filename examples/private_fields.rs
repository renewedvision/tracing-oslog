//! Demonstrates logging with private fields.
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
//! cargo run --example private_fields
//! ```

use std::fmt::Display;

use tracing_subscriber::prelude::*;

#[derive(Debug)]
struct UserInfo {
	id: u64,
	email: String,
}

impl Display for UserInfo {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "user#{} <{}>", self.id, self.email)
	}
}

fn main() {
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
		user = ?tracing_oslog::Private(&user),
		user_display = %tracing_oslog::Private(&user),
	);
}
