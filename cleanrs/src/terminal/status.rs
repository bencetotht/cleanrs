//! Best-effort OSC output. Never read terminal replies or interfere with input.
use std::io::{self, IsTerminal, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub(crate) enum Integration {
    #[default]
    Auto,
    Always,
    Never,
}

struct Sink {
    enabled: bool,
    tui: bool,
    tmux: bool,
    last: Option<(String, String, Instant)>,
}
static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

pub(crate) fn init(mode: Integration, tui: bool, json: bool) {
    let terminal = if tui {
        io::stdout().is_terminal()
    } else {
        io::stderr().is_terminal()
    };
    let enabled = !json
        && terminal
        && !matches!(mode, Integration::Never)
        && (matches!(mode, Integration::Always) || std::env::var("TERM").as_deref() != Ok("dumb"));
    let _ = SINK.set(Mutex::new(Sink {
        enabled,
        tui,
        tmux: std::env::var_os("TMUX").is_some(),
        last: None,
    }));
}

/// State changes bypass throttling so completion and attention cannot get lost.
pub(crate) fn report(state: &str, message: &str) {
    let Some(sink) = SINK.get() else { return };
    let Ok(mut sink) = sink.lock() else { return };
    if !sink.enabled {
        return;
    }
    let message = clean_text(message);
    if let Some((previous, text, time)) = &sink.last {
        if previous == state && (text == &message || time.elapsed() < Duration::from_millis(250)) {
            return;
        }
    }
    let bytes = sequences(state, &message, sink.tmux);
    // One stream lock prevents an OSC from being interleaved with a frame.
    let result = if sink.tui {
        let mut output = io::stdout().lock();
        output
            .write_all(bytes.as_bytes())
            .and_then(|_| output.flush())
    } else {
        let mut output = io::stderr().lock();
        output
            .write_all(bytes.as_bytes())
            .and_then(|_| output.flush())
    };
    if result.is_err() {
        sink.enabled = false;
    }
    sink.last = Some((state.into(), message, Instant::now()));
}

/// Preserve an incomplete-coverage error when the command otherwise succeeded.
pub(crate) fn finish(success: bool) {
    let failed = SINK.get().and_then(|s| s.lock().ok()).is_some_and(|s| {
        s.last
            .as_ref()
            .is_some_and(|(state, _, _)| state == "error")
    });
    if !success || !failed {
        report(
            if success { "done" } else { "error" },
            if success {
                "finished"
            } else {
                "command failed"
            },
        );
    }
}

fn clean_text(text: &str) -> String {
    // Bound before encoding; strip C0/C1 controls, including OSC terminators.
    text.chars().filter(|c| !c.is_control()).take(240).collect()
}

fn sequences(state: &str, message: &str, tmux: bool) -> String {
    let title = format!("cleanrs · {message}");
    let kind = if state == "blocked" {
        ":kind=question"
    } else {
        ""
    };
    let osc = format!(
        "\x1b]7501;state={state}:app=cleanrs{kind}:msg={}:title={}\x1b\\\x1b]2;{title}\x1b\\",
        base64(message.as_bytes()),
        base64(title.as_bytes())
    );
    if tmux {
        format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b"))
    } else {
        osc
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        output.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoding_and_injection_boundaries() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        let text = clean_text("path\x1b]2;injected\x07\n");
        assert!(!text.contains('\x1b'));
        assert!(sequences("working", &clean_text(&"界".repeat(10000)), false).len() < 4096);
        assert!(sequences("blocked", "Review", false).contains("app=cleanrs:kind=question"));
        assert!(sequences("idle", "idle", true).starts_with("\x1bPtmux;\x1b\x1b]7501;"));
    }
}
