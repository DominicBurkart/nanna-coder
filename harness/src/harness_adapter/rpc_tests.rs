use super::rpc::*;
use super::Endpoint;
use std::time::Duration;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

fn limits(line: usize, total: usize) -> ReadLimits {
    ReadLimits {
        max_line: line,
        max_total: total,
        read_timeout: Duration::from_secs(5),
    }
}

async fn reader_over(bytes: &[u8], limits: ReadLimits) -> LineReader<tokio::io::DuplexStream> {
    let (mut tx, rx) = duplex(1 << 20);
    tx.write_all(bytes).await.unwrap();
    drop(tx);
    LineReader::new(rx, limits)
}

#[tokio::test]
async fn lines_split_on_lf_only() {
    let mut r = reader_over(b"a\nb\r\nc\n", limits(16, 64)).await;
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("a"));
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("b\r"));
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("c"));
    assert!(r.next_line().await.unwrap().is_none());
}

#[tokio::test]
async fn a_lone_cr_and_the_unicode_separators_are_not_line_breaks() {
    let text = "x\ry\u{2028}z\u{2029}w\u{85}v";
    let mut r = reader_over(format!("{text}\nnext\n").as_bytes(), limits(64, 256)).await;
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some(text));
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("next"));
}

#[tokio::test]
async fn a_line_of_exactly_the_cap_passes_and_one_more_byte_fails() {
    let ok = format!("{}\n", "a".repeat(8));
    let mut r = reader_over(ok.as_bytes(), limits(8, 64)).await;
    assert_eq!(r.next_line().await.unwrap().unwrap().len(), 8);
    let long = format!("{}\n", "a".repeat(9));
    let mut r = reader_over(long.as_bytes(), limits(8, 64)).await;
    assert!(matches!(
        r.next_line().await,
        Err(RpcError::LineTooLong { limit: 8 })
    ));
    let mut r = reader_over("a".repeat(9).as_bytes(), limits(8, 64)).await;
    assert!(matches!(
        r.next_line().await,
        Err(RpcError::LineTooLong { limit: 8 })
    ));
}

#[tokio::test]
async fn a_huge_line_fails_before_it_is_buffered() {
    let big = vec![b'a'; 1 << 20];
    let mut r = reader_over(&big, limits(100, 1 << 30)).await;
    assert!(matches!(
        r.next_line().await,
        Err(RpcError::LineTooLong { .. })
    ));
}

#[tokio::test]
async fn the_total_cap_counts_every_consumed_byte() {
    let mut r = reader_over(b"aaaa\nbbbb\n", limits(8, 10)).await;
    assert!(r.next_line().await.is_ok());
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("bbbb"));
    let mut r = reader_over(b"aaaa\nbbbb\n", limits(8, 9)).await;
    assert!(r.next_line().await.is_ok());
    assert!(matches!(
        r.next_line().await,
        Err(RpcError::TotalTooLong { limit: 9 })
    ));
}

#[tokio::test]
async fn a_partial_final_line_and_bad_utf8_are_errors() {
    let mut r = reader_over(b"abc", limits(8, 64)).await;
    assert!(matches!(r.next_line().await, Err(RpcError::Truncated)));
    let mut r = reader_over(b"\xff\xfe\n", limits(8, 64)).await;
    assert!(matches!(r.next_line().await, Err(RpcError::NotUtf8)));
}

#[tokio::test]
async fn after_an_error_the_reader_stays_failed() {
    let mut r = reader_over(b"abc", limits(8, 64)).await;
    assert!(r.next_line().await.is_err());
    assert!(matches!(r.next_line().await, Err(RpcError::Failed)));
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_times_out() {
    let (tx, rx) = duplex(64);
    let mut r = LineReader::new(rx, limits(8, 64));
    assert!(matches!(r.next_line().await, Err(RpcError::Timeout)));
    drop(tx);
}

#[tokio::test]
async fn a_line_split_across_reads_is_reassembled() {
    let (mut tx, rx) = duplex(4);
    let mut r = LineReader::new(rx, limits(64, 256));
    let writer = tokio::spawn(async move {
        for chunk in ["he", "llo ", "wor", "ld\n"] {
            tx.write_all(chunk.as_bytes()).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    assert_eq!(r.next_line().await.unwrap().as_deref(), Some("hello world"));
    writer.await.unwrap();
}

#[test]
fn the_default_limits_are_pinned() {
    let r = ReadLimits::for_requests();
    assert_eq!(r.max_line, 1_048_576);
    assert_eq!(r.max_total, 1_048_577);
    assert_eq!(r.read_timeout, REQUEST_READ_TIMEOUT);
    let e = ReadLimits::for_events(Duration::from_secs(3));
    assert_eq!(e.max_line, MAX_RPC_LINE_BYTES);
    assert_eq!(e.max_total, MAX_RPC_TOTAL_BYTES);
    assert_eq!(e.read_timeout, Duration::from_secs(3));
}

#[tokio::test]
async fn replies_escape_the_separators_and_end_in_one_lf() {
    let (mut tx, mut rx) = duplex(1024);
    write_reply(
        &mut tx,
        "{\"a\":\"\u{2028}\u{2029}\"}",
        1024,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    drop(tx);
    let mut out = String::new();
    rx.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "{\"a\":\"\\u2028\\u2029\"}\n");
}

#[tokio::test]
async fn a_reply_with_a_line_feed_or_over_the_cap_is_refused() {
    let (mut tx, _rx) = duplex(1024);
    assert!(matches!(
        write_reply(&mut tx, "a\nb", 64, Duration::from_secs(1)).await,
        Err(RpcError::ReplyHasNewline)
    ));
    assert!(write_reply(&mut tx, "abcd", 5, Duration::from_secs(1))
        .await
        .is_ok());
    assert!(matches!(
        write_reply(&mut tx, "abcde", 5, Duration::from_secs(1)).await,
        Err(RpcError::ReplyTooLarge { limit: 5 })
    ));
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_never_reads_times_out_the_write() {
    let (mut tx, _rx) = duplex(4);
    assert!(matches!(
        write_reply(&mut tx, "0123456789", 64, Duration::from_secs(1)).await,
        Err(RpcError::Timeout)
    ));
}

#[test]
fn the_relay_endpoint_names_the_podman_host_alias() {
    let e = Endpoint::in_container_relay(8080).unwrap();
    assert_eq!(e.host(), "host.containers.internal");
    assert_eq!(e.port(), 8080);
    assert!(Endpoint::in_container_relay(0).is_err());
    assert_eq!(RELAY_HOST, "host.containers.internal");
    assert_eq!(MAX_REPLY_BYTES, 1_048_576);
}
