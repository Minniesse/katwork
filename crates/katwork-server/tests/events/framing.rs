use eventsource_stream::Eventsource;
use futures::{StreamExt, stream};
use katwork::events::AgUiEvent;

const FIXTURE: &[u8] = include_bytes!("../fixtures/sse/thai_emoji.sse");

async fn decode(chunks: Vec<Vec<u8>>) -> Vec<eventsource_stream::Event> {
    stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>))
        .eventsource()
        .map(|result| result.unwrap())
        .collect()
        .await
}

#[tokio::test]
async fn sse_thai_and_emoji_event_parses_at_every_byte_split() {
    for split in 0..=FIXTURE.len() {
        let events = decode(vec![FIXTURE[..split].to_vec(), FIXTURE[split..].to_vec()]).await;
        assert_eq!(events.len(), 5, "byte split {split}");
        let delta: AgUiEvent = serde_json::from_str(&events[2].data).unwrap();
        assert!(
            matches!(delta, AgUiEvent::TextMessageContent { delta, .. } if delta == "ที่นี่สวัสดีครับ 👋🏽 🇹🇭")
        );
        assert_eq!(
            events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["1", "2", "3", "4", "5"]
        );
    }
}

#[test]
fn sse_fixture_matches_the_web_fixture() {
    assert_eq!(
        FIXTURE,
        include_bytes!("../../../../web/src/testing/fixtures/thai_emoji.sse")
    );
}

#[tokio::test]
async fn sse_crlf_framing_and_comment_keepalives_parse() {
    let mut crlf = Vec::new();
    for byte in FIXTURE {
        if *byte == b'\n' {
            crlf.push(b'\r');
        }
        crlf.push(*byte);
    }
    let parsed = decode(crlf.iter().map(|b| vec![*b]).collect()).await;
    let expected = decode(vec![FIXTURE.to_vec()]).await;
    assert_eq!(parsed, expected);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn server_frames_roundtrip_through_the_fixture_parser(pool: sqlx::PgPool) {
    let s = super::Setup::new(&pool).await;
    s.append(1, false).await;
    let mut body = s.stream(None).await;
    let mut chunks = vec![super::next(&mut body).await.to_vec()];
    chunks.push(super::next(&mut body).await.to_vec());
    let events = decode(chunks).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "1");
    assert!(
        matches!(serde_json::from_str::<AgUiEvent>(&events[0].data).unwrap(), AgUiEvent::TextMessageContent { delta, .. } if delta == "สวัสดี 👋🏽")
    );
}
