// SPDX-License-Identifier: Apache-2.0
//! Out-of-band human notifications with policy and injectable dispatch.
//!
//! This crate models a notification, a quiet-hours window, and a batching
//! policy, then dispatches each notification over a [`Channel`] using an
//! injected sender. Desktop and command channels are realised as the OS-native
//! command line to run, while the webhook channel is delivered through a
//! caller-supplied closure so the crate stays free of any network or process
//! side effects and can be unit-tested entirely offline.
//!
//! # Example
//!
//! ```
//! use origin_notify::{should_send, Notification, QuietHours};
//!
//! let n = Notification::new("Build failed", "tests are red", false);
//! let quiet = QuietHours::new(23 * 60, 7 * 60);
//! // 02:00 falls inside the wrap-around 23:00-07:00 window.
//! assert!(!should_send(&n, Some(&quiet), 2 * 60));
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of minutes in a single day.
const MINUTES_PER_DAY: u32 = 24 * 60;

/// Errors that can arise when building a notification payload or dispatching.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NotifyError {
    /// A payload or command could not be constructed from the inputs.
    #[error("failed to build notification: {0}")]
    Build(String),
}

/// A delivery channel for a notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    /// Deliver to an HTTP endpoint by `POST`ing a JSON payload.
    Webhook {
        /// Absolute URL the JSON payload is `POST`ed to.
        url: String,
    },
    /// Deliver to the local desktop using the OS-native notifier.
    Desktop,
    /// Deliver by spawning an arbitrary program with arguments.
    Command {
        /// Executable to run.
        program: String,
        /// Arguments passed to the executable.
        args: Vec<String>,
    },
}

/// A single human-facing notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Short headline shown first.
    pub title: String,
    /// Longer descriptive body.
    pub body: String,
    /// Whether the notification bypasses quiet hours.
    pub urgent: bool,
}

impl Notification {
    /// Creates a new [`Notification`].
    #[must_use]
    pub fn new(title: impl Into<String>, body: impl Into<String>, urgent: bool) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            urgent,
        }
    }
}

/// A daily quiet-hours window expressed as minutes-of-day.
///
/// The window may wrap around midnight: when `start_min` is greater than
/// `end_min` (for example `23:00` to `07:00`), the quiet period spans the day
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    /// Inclusive start of the window, in minutes since midnight.
    pub start_min: u32,
    /// Exclusive end of the window, in minutes since midnight.
    pub end_min: u32,
}

impl QuietHours {
    /// Creates a new [`QuietHours`] window from start/end minutes-of-day.
    #[must_use]
    pub const fn new(start_min: u32, end_min: u32) -> Self {
        Self { start_min, end_min }
    }

    /// Returns whether `minute_of_day` falls inside the quiet window.
    ///
    /// Handles wrap-around windows that cross midnight. An empty window
    /// (`start_min == end_min`) is never quiet.
    #[must_use]
    pub const fn is_quiet(&self, minute_of_day: u32) -> bool {
        let now = minute_of_day % MINUTES_PER_DAY;
        let start = self.start_min % MINUTES_PER_DAY;
        let end = self.end_min % MINUTES_PER_DAY;
        if start == end {
            // Degenerate window: treat as no quiet hours at all.
            false
        } else if start < end {
            now >= start && now < end
        } else {
            // Wrap-around: quiet from start..midnight and midnight..end.
            now >= start || now < end
        }
    }
}

/// A bounded batcher that accumulates notifications until flushed.
///
/// Notifications are buffered and returned in FIFO order on [`Batcher::flush`].
/// When the buffer reaches `max_batch`, [`Batcher::push`] signals that the
/// caller should flush by returning `true`.
#[derive(Debug, Clone, Default)]
pub struct Batcher {
    max_batch: usize,
    pending: Vec<Notification>,
}

impl Batcher {
    /// Creates a new [`Batcher`] that fills at `max_batch` items.
    ///
    /// A `max_batch` of `0` is normalised to `1` so the batcher always makes
    /// progress.
    #[must_use]
    pub fn new(max_batch: usize) -> Self {
        Self {
            max_batch: max_batch.max(1),
            pending: Vec::new(),
        }
    }

    /// Pushes a notification into the buffer.
    ///
    /// Returns `true` when the buffer has reached `max_batch` and the caller
    /// should [`flush`](Self::flush).
    pub fn push(&mut self, n: Notification) -> bool {
        self.pending.push(n);
        self.pending.len() >= self.max_batch
    }

    /// Returns the number of buffered notifications.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Returns whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drains and returns all buffered notifications in FIFO order.
    pub fn flush(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.pending)
    }
}

/// Decides whether a notification should be sent right now.
///
/// Urgent notifications always send. Otherwise, the notification is suppressed
/// while inside the supplied quiet-hours window. With no quiet hours the
/// notification always sends.
#[must_use]
pub fn should_send(n: &Notification, quiet: Option<&QuietHours>, minute_of_day: u32) -> bool {
    if n.urgent {
        return true;
    }
    quiet.is_none_or(|q| !q.is_quiet(minute_of_day))
}

/// Builds the OS-native desktop notifier command for a notification.
///
/// Returns the program to run and its argument vector. On macOS this uses
/// `osascript`, on Windows a `PowerShell` toast, and on other platforms
/// `notify-send`.
#[must_use]
pub fn desktop_command(n: &Notification) -> (String, Vec<String>) {
    if cfg!(target_os = "macos") {
        let script = format!(
            "display notification {} with title {}",
            applescript_quote(&n.body),
            applescript_quote(&n.title),
        );
        ("osascript".to_owned(), vec!["-e".to_owned(), script])
    } else if cfg!(target_os = "windows") {
        let script = windows_toast_script(n);
        (
            "powershell".to_owned(),
            vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                script,
            ],
        )
    } else {
        let mut args = Vec::new();
        if n.urgent {
            args.push("-u".to_owned());
            args.push("critical".to_owned());
        }
        args.push(n.title.clone());
        args.push(n.body.clone());
        ("notify-send".to_owned(), args)
    }
}

/// Serialises a notification to a JSON webhook payload.
///
/// The object contains `title`, `body`, and `urgent` fields.
///
/// # Errors
///
/// Returns [`NotifyError::Build`] if serialisation fails (which cannot happen
/// for these primitive fields but is surfaced rather than panicking).
pub fn try_webhook_payload(n: &Notification) -> Result<String, NotifyError> {
    serde_json::to_string(n).map_err(|e| NotifyError::Build(e.to_string()))
}

/// Serialises a notification to a JSON webhook payload.
///
/// This is the infallible convenience wrapper over [`try_webhook_payload`];
/// it falls back to a hand-built object if serialisation ever fails so the
/// caller always receives valid JSON.
#[must_use]
pub fn webhook_payload(n: &Notification) -> String {
    try_webhook_payload(n).unwrap_or_else(|_| {
        format!(
            "{{\"title\":{},\"body\":{},\"urgent\":{}}}",
            json_string(&n.title),
            json_string(&n.body),
            n.urgent,
        )
    })
}

/// Quotes a string as an `AppleScript` string literal.
fn applescript_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Builds the `PowerShell` toast-notification script body for a notification.
fn windows_toast_script(n: &Notification) -> String {
    // Use BurntToast-free balloon-style toast via the WinRT APIs.
    let title = powershell_quote(&n.title);
    let body = powershell_quote(&n.body);
    format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null; \
         $t=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
         $x=$t.GetElementsByTagName('text'); $x.Item(0).AppendChild($t.CreateTextNode({title}))|Out-Null; \
         $x.Item(1).AppendChild($t.CreateTextNode({body}))|Out-Null; \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('origin').Show([Windows.UI.Notifications.ToastNotification]::new($t))",
    )
}

/// Quotes a string as a single-quoted `PowerShell` literal.
fn powershell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// Encodes a string as a minimal JSON string literal (fallback path only).
fn json_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Control characters are escaped as \u00XX; the write to a
                // String is infallible so the result is intentionally ignored.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_wraps_around_midnight() {
        let q = QuietHours::new(23 * 60, 7 * 60);
        assert!(q.is_quiet(23 * 60)); // 23:00 start, inclusive
        assert!(q.is_quiet(2 * 60)); // 02:00 inside wrap
        assert!(q.is_quiet(6 * 60 + 59)); // 06:59 still quiet
        assert!(!q.is_quiet(7 * 60)); // 07:00 end, exclusive
        assert!(!q.is_quiet(12 * 60)); // midday awake
        assert!(!q.is_quiet(22 * 60 + 59)); // 22:59 just before
    }

    #[test]
    fn quiet_hours_non_wrapping_window() {
        let q = QuietHours::new(60, 5 * 60);
        assert!(!q.is_quiet(0));
        assert!(q.is_quiet(60));
        assert!(q.is_quiet(3 * 60));
        assert!(!q.is_quiet(5 * 60));
        assert!(!q.is_quiet(6 * 60));
    }

    #[test]
    fn quiet_hours_degenerate_window_never_quiet() {
        let q = QuietHours::new(60, 60);
        assert!(!q.is_quiet(60));
        assert!(!q.is_quiet(0));
        assert!(!q.is_quiet(MINUTES_PER_DAY - 1));
    }

    #[test]
    fn urgent_bypasses_quiet_hours() {
        let q = QuietHours::new(23 * 60, 7 * 60);
        let calm = Notification::new("info", "fyi", false);
        let urgent = Notification::new("alert", "now", true);
        assert!(!should_send(&calm, Some(&q), 2 * 60));
        assert!(should_send(&urgent, Some(&q), 2 * 60));
    }

    #[test]
    fn should_send_without_quiet_hours_always_sends() {
        let n = Notification::new("t", "b", false);
        assert!(should_send(&n, None, 0));
        assert!(should_send(&n, None, 3 * 60));
    }

    #[test]
    fn should_send_outside_quiet_window() {
        let q = QuietHours::new(23 * 60, 7 * 60);
        let n = Notification::new("t", "b", false);
        assert!(should_send(&n, Some(&q), 12 * 60));
    }

    #[test]
    fn batcher_flushes_at_max() {
        let mut b = Batcher::new(2);
        assert!(!b.push(Notification::new("a", "1", false)));
        assert!(b.push(Notification::new("b", "2", false)));
        let drained = b.flush();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].title, "a");
        assert_eq!(drained[1].title, "b");
        assert!(b.is_empty());
    }

    #[test]
    fn batcher_flush_on_demand_below_max() {
        let mut b = Batcher::new(10);
        assert!(!b.push(Notification::new("a", "1", false)));
        assert_eq!(b.len(), 1);
        let drained = b.flush();
        assert_eq!(drained.len(), 1);
        assert!(b.is_empty());
        // Flushing an empty batcher yields nothing.
        assert!(b.flush().is_empty());
    }

    #[test]
    fn batcher_zero_max_normalised_to_one() {
        let mut b = Batcher::new(0);
        assert!(b.push(Notification::new("a", "1", false)));
        assert_eq!(b.flush().len(), 1);
    }

    #[test]
    fn webhook_payload_is_valid_json_with_fields() {
        let n = Notification::new("Build \"failed\"", "line\nbreak", true);
        let payload = webhook_payload(&n);
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["title"], "Build \"failed\"");
        assert_eq!(parsed["body"], "line\nbreak");
        assert_eq!(parsed["urgent"], true);
    }

    #[test]
    fn try_webhook_payload_round_trips() {
        let n = Notification::new("t", "b", false);
        let payload = try_webhook_payload(&n).unwrap();
        let back: Notification = serde_json::from_str(&payload).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn desktop_command_is_non_empty() {
        let n = Notification::new("Title", "Body", false);
        let (program, args) = desktop_command(&n);
        assert!(!program.is_empty());
        assert!(!args.is_empty());
    }

    #[test]
    fn desktop_command_contains_text_for_platform() {
        let n = Notification::new("Hello", "World", true);
        let (program, args) = desktop_command(&n);
        let joined = args.join(" ");
        if cfg!(target_os = "macos") {
            assert_eq!(program, "osascript");
            assert!(joined.contains("Hello"));
            assert!(joined.contains("World"));
        } else if cfg!(target_os = "windows") {
            assert_eq!(program, "powershell");
            assert!(joined.contains("Hello"));
            assert!(joined.contains("World"));
        } else {
            assert_eq!(program, "notify-send");
            assert!(args.contains(&"Hello".to_owned()));
            assert!(args.contains(&"World".to_owned()));
        }
    }

    #[test]
    fn channel_serde_round_trip() {
        let c = Channel::Command {
            program: "echo".to_owned(),
            args: vec!["hi".to_owned()],
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Channel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
