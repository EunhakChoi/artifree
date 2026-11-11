//! Tests for `read_lines`, which reads a supervised worker's stdout as lines
//! (decision 0055, rule 6). Each test reads through a slice or an in-memory
//! pipe and checks the lines reported, in order.

use std::error::Error;

use artifree_server::worker_lines::{WorkerLine, read_lines};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWriteExt, DuplexStream};

type TestResult = Result<(), Box<dyn Error>>;

/// The line limit of rule 6, in bytes without the newline.
const LIMIT: usize = 64 * 1024;

/// Collects every line `read_lines` reports from `reader`, in order.
async fn collect_lines<R: AsyncRead + Unpin>(reader: R) -> Vec<WorkerLine> {
    let mut lines = Vec::new();
    read_lines(reader, |line| lines.push(line)).await;
    lines
}

/// Writes `input` in pieces of `piece` bytes, then shuts the writer down.
async fn write_in_pieces(
    writer: &mut DuplexStream,
    input: &[u8],
    piece: usize,
) -> std::io::Result<()> {
    for chunk in input.chunks(piece) {
        writer.write_all(chunk).await?;
    }
    writer.shutdown().await
}

/// Reads `input` through an in-memory pipe that is written in pieces of
/// `piece` bytes, and returns the lines `read_lines` reports.
async fn lines_through_pipe(input: &[u8], piece: usize) -> std::io::Result<Vec<WorkerLine>> {
    let (mut writer, reader) = tokio::io::duplex(256);
    let (written, lines) = tokio::join!(
        write_in_pieces(&mut writer, input, piece),
        collect_lines(reader),
    );
    written?;
    Ok(lines)
}

/// A JSON string literal of `total_len` bytes, without a newline. The quotes
/// count toward `total_len`, which must be at least 2.
fn string_line(total_len: usize) -> Vec<u8> {
    format!("\"{}\"", "a".repeat(total_len - 2)).into_bytes()
}

#[tokio::test]
async fn json_lines_are_reported_as_their_values() -> TestResult {
    let kinds: [(&str, &[u8], Value); 5] = [
        ("object", b"{\"a\":1}\n", json!({"a": 1})),
        ("array", b"[1,2]\n", json!([1, 2])),
        ("string", b"\"text\"\n", json!("text")),
        ("number", b"7\n", json!(7)),
        ("null", b"null\n", Value::Null),
    ];
    for (kind, input, value) in kinds {
        assert_eq!(
            collect_lines(input).await,
            [WorkerLine::Json(value)],
            "the {kind} line must be reported as its JSON value"
        );
    }
    Ok(())
}

#[tokio::test]
async fn lines_that_are_not_json_are_dropped() -> TestResult {
    let kinds: [(&str, &[u8]); 3] = [
        ("plain text", b"not json\n"),
        ("empty", b"\n"),
        ("truncated object", b"{\"open\":\n"),
    ];
    for (kind, input) in kinds {
        assert_eq!(
            collect_lines(input).await,
            [WorkerLine::Dropped],
            "the {kind} line must be reported as one dropped line"
        );
    }
    Ok(())
}

#[tokio::test]
async fn line_of_exactly_limit_bytes_is_kept() -> TestResult {
    let mut input = string_line(LIMIT);
    assert_eq!(
        input.len(),
        LIMIT,
        "the fixture must be exactly LIMIT bytes"
    );
    input.push(b'\n');
    assert_eq!(
        collect_lines(input.as_slice()).await,
        [WorkerLine::Json(Value::String("a".repeat(LIMIT - 2)))],
        "a valid JSON line of exactly LIMIT bytes must be kept"
    );
    Ok(())
}

#[tokio::test]
async fn line_one_byte_over_the_limit_is_dropped() -> TestResult {
    let mut input = string_line(LIMIT + 1);
    assert_eq!(
        input.len(),
        LIMIT + 1,
        "the fixture must be one byte over LIMIT"
    );
    input.push(b'\n');
    // The line is valid JSON, so only its length can make it dropped.
    assert_eq!(
        collect_lines(input.as_slice()).await,
        [WorkerLine::Dropped],
        "a valid JSON line one byte over LIMIT must be dropped"
    );
    Ok(())
}

#[tokio::test]
async fn overlong_line_is_dropped_and_its_tail_is_never_read() -> TestResult {
    // The padding runs well past the limit, so the tail starts in a later read
    // than the one that crosses it. The tail is a complete JSON object: a reader
    // that resumed after the overlong bytes would report it as a line.
    let mut padded = vec![b' '; 2 * LIMIT];
    padded.extend_from_slice(b"{\"tail\":true}");
    let kinds: [(&str, Vec<u8>); 2] = [
        ("JSON string", string_line(LIMIT + 1)),
        ("whitespace then JSON object", padded),
    ];
    for (kind, mut overlong) in kinds {
        overlong.extend_from_slice(b"\n{\"next\":1}\n");
        assert_eq!(
            collect_lines(overlong.as_slice()).await,
            [WorkerLine::Dropped, WorkerLine::Json(json!({"next": 1}))],
            "the {kind} line must be one dropped line, with its tail never read"
        );
    }
    Ok(())
}

#[tokio::test]
async fn seven_byte_pieces_report_the_same_lines_as_one_write() -> TestResult {
    let mut input = b"{\"a\":1}\nnot json\n\n".to_vec();
    input.extend(string_line(LIMIT + 1));
    input.extend(b"\n{\"b\":2}");
    let expected = [
        WorkerLine::Json(json!({"a": 1})),
        WorkerLine::Dropped,
        WorkerLine::Dropped,
        WorkerLine::Dropped,
        WorkerLine::Json(json!({"b": 2})),
    ];
    let whole = lines_through_pipe(&input, input.len()).await?;
    let pieces = lines_through_pipe(&input, 7).await?;
    assert_eq!(
        whole, expected,
        "one write must report each line once, in order"
    );
    assert_eq!(
        pieces, whole,
        "7-byte pieces must report the same lines as one write"
    );
    Ok(())
}

#[tokio::test]
async fn end_of_input_closes_the_last_line() -> TestResult {
    let kinds: [(&str, Vec<u8>, Vec<WorkerLine>); 4] = [
        (
            "JSON line without a newline",
            b"{\"last\":true}".to_vec(),
            vec![WorkerLine::Json(json!({"last": true}))],
        ),
        (
            "text without a newline",
            b"not json".to_vec(),
            vec![WorkerLine::Dropped],
        ),
        (
            "overlong line without a newline",
            string_line(LIMIT + 1),
            vec![WorkerLine::Dropped],
        ),
        ("empty input", Vec::new(), Vec::new()),
    ];
    for (kind, input, expected) in kinds {
        assert_eq!(
            collect_lines(input.as_slice()).await,
            expected,
            "{kind}: the reported lines must match"
        );
    }
    Ok(())
}
