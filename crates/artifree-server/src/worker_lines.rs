//! Reads a supervised worker's standard output (decision 0055, rule 6).

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// The longest line kept, in bytes without its newline: 64 KiB.
pub const MAX_LINE: usize = 64 * 1024;

/// One line the worker printed.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerLine {
    /// A line that parsed as JSON.
    Json(serde_json::Value),
    /// A line that did not parse, was longer than `MAX_LINE`, or was cut off by
    /// a read error. Its content is gone.
    Dropped,
}

/// Reads `reader` to its end or first read error and reports each line in order.
///
/// A line is reported when its newline, the end of the input, or a read error
/// arrives. A line longer than `MAX_LINE` is reported as
/// [`WorkerLine::Dropped`]: its bytes are never buffered past the limit, and it
/// never splits into the next line. A last line without a newline counts as a
/// line. A partial line at a read error is reported as dropped, and nothing is
/// read after that error. This function never logs content and never panics.
pub async fn read_lines<R: AsyncRead + Unpin>(reader: R, mut on_line: impl FnMut(WorkerLine)) {
    let mut reader = BufReader::new(reader);
    let mut line: Vec<u8> = Vec::new();
    let mut overlong = false;
    loop {
        let (consumed, found) = match reader.fill_buf().await {
            Err(_) => {
                if overlong || !line.is_empty() {
                    on_line(WorkerLine::Dropped);
                }
                return;
            }
            Ok([]) => {
                if overlong || !line.is_empty() {
                    on_line(classify(&line, overlong));
                }
                return;
            }
            Ok(available) => {
                let newline = available.iter().position(|byte| *byte == b'\n');
                let content_len = newline.unwrap_or(available.len());
                // `content_len` never exceeds the slice, so `get` always succeeds.
                let content = available.get(..content_len).unwrap_or_default();
                if !overlong {
                    if line.len() + content.len() > MAX_LINE {
                        overlong = true;
                        line.clear();
                    } else {
                        line.extend_from_slice(content);
                    }
                }
                let has_newline = newline.is_some();
                (content.len() + usize::from(has_newline), has_newline)
            }
        };
        reader.consume(consumed);
        if found {
            on_line(classify(&line, overlong));
            line.clear();
            overlong = false;
        }
    }
}

/// Turns a finished line into the value reported for it.
fn classify(line: &[u8], overlong: bool) -> WorkerLine {
    if overlong {
        return WorkerLine::Dropped;
    }
    let parsed: Result<serde_json::Value, serde_json::Error> = serde_json::from_slice(line);
    parsed.map_or(WorkerLine::Dropped, WorkerLine::Json)
}
