use katwork::{
    chat::{Author, Message},
    ids::*,
    models::ChatMsg,
    pipeline::build_history,
};

fn message(author: Author, name: &str, body: &str) -> Message {
    Message {
        id: MessageId::new(),
        thread_id: ThreadId::new(),
        author,
        author_name: name.into(),
        body: body.into(),
        synthetic: false,
        stopped: false,
        question: None,
        meta: None,
        job_id: None,
        job_status: None,
        job_reason: None,
        client_msg_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn budget_cut_drops_leading_assistant_and_merges_user_turns() {
    let kat = KatId::new();
    let messages = [
        message(Author::User(UserId::new()), "Person", &"x".repeat(100)),
        message(Author::Kat(kat), "Writer", "old reply"),
        message(Author::Kat(KatId::new()), "Researcher", "context"),
        message(Author::User(UserId::new()), "Person", "request"),
    ];
    let history = build_history(&messages, kat, 44);
    assert_eq!(history.cut, 2);
    assert_eq!(
        history.messages,
        vec![ChatMsg::User {
            content: "Researcher: context\nPerson: request".into(),
            name: None,
        }]
    );
}

#[test]
fn merged_author_labels_fit_the_history_budget() {
    let messages = [
        message(Author::User(UserId::new()), "Researcher", "context"),
        message(Author::User(UserId::new()), "Person", "request"),
    ];
    let history = build_history(&messages, KatId::new(), 23);
    assert_eq!(history.cut, 1);
    assert_eq!(
        history.messages,
        vec![ChatMsg::User {
            content: "request".into(),
            name: Some("Person".into()),
        }]
    );
}
