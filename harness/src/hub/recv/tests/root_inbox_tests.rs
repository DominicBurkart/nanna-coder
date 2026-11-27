use super::*;
use crate::hub::mailbox::MailboxId;

const CLEAN: &str = "build is green";
const BLOCKED: &str = "token=abc";
const CONTENT: &str = "my .env has\nPORT=1\nHOST=x\nMODE=y";

fn mailbox_of(root: &RootId) -> SessionId {
    MailboxId::for_root(root).unwrap().into_session()
}

fn mail(from: &SessionBinding, subject: &str, body: &str, state: DeliveryState) -> StoredMessage {
    let mut message = stored(from, from, subject, body, state);
    message.to = mailbox_of(from.lineage().root());
    message
}

fn put_state(world: &World, from: &SessionBinding, body: &str, state: DeliveryState) -> MessageId {
    let message = mail(from, "status", body, state);
    let id = message.id;
    world.messages.append(message).unwrap();
    id
}

fn put_mail(world: &World, from: &SessionBinding, body: &str) -> MessageId {
    put_state(world, from, body, DeliveryState::Pending)
}

fn state_of(world: &World, id: MessageId) -> DeliveryState {
    world.messages.get(id).unwrap().unwrap().state
}

fn texts(page: &RootPage) -> Vec<&str> {
    page.messages
        .iter()
        .map(|message| inner_text(&message.body))
        .collect()
}

fn ask(max: u8) -> RootRequest {
    RootRequest {
        max,
        ..RootRequest::default()
    }
}

fn other_root() -> RootId {
    RootId::mcp(Uuid::from_u128(2))
}

fn notice() -> String {
    format!(
        "{} {}",
        untrusted_notice_as(FenceLabel::AgentMessage),
        untrusted_notice_as(FenceLabel::AgentText)
    )
}

fn json_of(page: &RootPage) -> String {
    serde_json::to_string(page).unwrap()
}

#[test]
fn mail_comes_back_oldest_first_marked_delivered_over_both_stores() {
    for (store, _, _guard) in stores() {
        let world = World::new(store);
        let a = world.seat("a");
        let recorder = Recorder::new(Say::Fail);
        let hub = world.hub(recorder.clone());
        let first = put_mail(&world, &a, "one");
        let second = put_mail(&world, &a, "two");
        let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert_eq!(texts(&page), ["one", "two"]);
        assert_eq!((page.withheld, page.remaining), (0, 0));
        assert_eq!(page.notice, notice());
        assert_eq!(page.messages[0].id, first);
        assert_eq!(page.messages[1].id, second);
        assert_eq!(page.messages[0].from, *a.session());
        assert_eq!(page.messages[0].from_depth, Some(1));
        assert!(!page.messages[0].released_by_human);
        assert_eq!(state_of(&world, first), DeliveryState::Delivered);
        assert_eq!(state_of(&world, second), DeliveryState::Delivered);
        assert!(recorder.seen().is_empty());
    }
}

#[test]
fn an_unknown_sender_is_shown_without_a_depth() {
    let world = World::memory();
    let a = world.seat("a");
    let ghost = world.bare_binding("ghost", EffectClass::None);
    put_mail(&world, &ghost, "from nowhere");
    put_mail(&world, &a, "from a");
    let hub = world.hub(Recorder::deliver());
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    assert_eq!(page.messages[0].from_depth, None);
    assert_eq!(page.messages[1].from_depth, Some(1));
}

#[test]
fn delivered_mail_is_shown_again_until_it_is_acknowledged_or_the_cursor_passes_it() {
    let world = World::memory();
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let ids = [
        put_mail(&world, &a, "one"),
        put_mail(&world, &a, "two"),
        put_mail(&world, &a, "three"),
    ];
    let first = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    let again = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    assert_eq!(texts(&first), texts(&again));
    assert_eq!(again.messages.len(), 3);
    let cursor = Some(first.messages[0].seq);
    let past = RootRequest {
        after: cursor,
        ..RootRequest::default()
    };
    let skipped = hub.root_inbox(&world.root, past).unwrap();
    assert_eq!(texts(&skipped), ["two", "three"]);
    let acking = RootRequest {
        ack_through: Some(first.messages[1].seq),
        ..RootRequest::default()
    };
    let rest = hub.root_inbox(&world.root, acking).unwrap();
    assert_eq!(texts(&rest), ["three"]);
    assert_eq!(state_of(&world, ids[0]), DeliveryState::Acked);
    assert_eq!(state_of(&world, ids[1]), DeliveryState::Acked);
    assert_eq!(state_of(&world, ids[2]), DeliveryState::Delivered);
}

#[test]
fn ack_through_acknowledges_only_this_mailbox() {
    for (store, _, _guard) in stores() {
        let world = World::new(store);
        let a = world.seat("a");
        let b = world.seat("b");
        let x = world.seat_in(&other_root(), "x", EffectClass::None);
        let hub = world.hub(Recorder::deliver());
        let own = put_state(&world, &a, "own", DeliveryState::Delivered);
        let foreign = put_state(&world, &x, "foreign", DeliveryState::Delivered);
        let session = world.put_as(&a, &b, "to b", DeliveryState::Delivered);
        let upto = Some(Seq::new(1000));
        let req = RootRequest {
            ack_through: upto,
            ..RootRequest::default()
        };
        let page = hub.root_inbox(&world.root, req).unwrap();
        assert!(page.messages.is_empty());
        assert_eq!(state_of(&world, own), DeliveryState::Acked);
        assert_eq!(state_of(&world, foreign), DeliveryState::Delivered);
        assert_eq!(state_of(&world, session), DeliveryState::Delivered);
    }
}

#[test]
fn each_root_reads_only_its_own_mailbox_and_empty_roots_look_alike() {
    let world = World::memory();
    let a = world.seat("a");
    let x = world.seat_in(&other_root(), "x", EffectClass::None);
    let hub = world.hub(Recorder::deliver());
    put_mail(&world, &a, "for root one");
    put_mail(&world, &x, "for root two");
    let one = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    let two = hub.root_inbox(&other_root(), RootRequest::default());
    assert_eq!(texts(&one), ["for root one"]);
    assert_eq!(texts(&two.unwrap()), ["for root two"]);
    let third = RootId::mcp(Uuid::from_u128(3));
    let fourth = RootId::mcp(Uuid::from_u128(4));
    let empty_a = hub.root_inbox(&third, RootRequest::default()).unwrap();
    let empty_b = hub.root_inbox(&fourth, RootRequest::default()).unwrap();
    assert_eq!(json_of(&empty_a), json_of(&empty_b));
    assert_eq!(format!("{empty_a:?}"), format!("{empty_b:?}"));
    assert!(empty_a.messages.is_empty());
}

#[test]
fn a_root_without_a_mailbox_is_not_visible_and_reads_nothing() {
    let world = World::memory();
    let hub = world.hub(Recorder::deliver());
    let forged = RootId::Plan {
        id: "NOT VALID".into(),
    };
    let refused = hub.root_inbox(&forged, RootRequest::default());
    assert!(matches!(refused, Err(HubError::NotVisible)));
}

fn foreign_lies(world: &World, a: &SessionBinding, b: &SessionBinding) -> Vec<StoredMessage> {
    let x = world.seat_in(&other_root(), "x", EffectClass::None);
    let mut other_root_mail = mail(&x, "s", "from-another-root", DeliveryState::Pending);
    other_root_mail.to = mailbox_of(&world.root);
    let mut other_mailbox = mail(a, "s", "to-another-mailbox", DeliveryState::Pending);
    other_mailbox.to = mailbox_of(&other_root());
    let mut to_session = mail(a, "s", "to-a-session", DeliveryState::Pending);
    to_session.to = b.session().clone();
    let mut lies = vec![other_root_mail, other_mailbox, to_session];
    let states = [
        DeliveryState::Acked,
        DeliveryState::Withheld,
        DeliveryState::Quarantined,
        DeliveryState::Expired,
    ];
    for state in states {
        lies.push(mail(a, "s", "closed-state", state));
    }
    lies
}

#[test]
fn a_store_that_hands_back_foreign_or_closed_messages_delivers_none_of_them() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let (a, b) = (world.seat("a"), world.seat("b"));
    let hub = world.hub(Recorder::deliver());
    let mut lies = foreign_lies(&world, &a, &b);
    let valid = mail(&a, "s", "the-valid-one", DeliveryState::Pending);
    let id = valid.id;
    world.messages.append(valid).unwrap();
    let kept = world.messages.get(id).unwrap().unwrap();
    lies.push(kept.clone());
    lies.push(kept);
    *flaky.lie.lock().unwrap() = lies;
    let page = hub.root_inbox(&world.root, ask(20)).unwrap();
    assert_eq!(texts(&page), ["the-valid-one"]);
    assert_eq!((page.withheld, page.remaining), (0, 0));
    assert_eq!(flaky.set_states.load(Ordering::SeqCst), 1);
}

#[test]
fn a_store_that_ignores_the_cursor_cannot_replay_older_mail() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let old = put_mail(&world, &a, "old");
    let new = put_mail(&world, &a, "new");
    let kept_old = world.messages.get(old).unwrap().unwrap();
    let kept_new = world.messages.get(new).unwrap().unwrap();
    let floor = Some(kept_old.seq);
    *flaky.lie.lock().unwrap() = vec![kept_old, kept_new];
    let req = RootRequest {
        after: floor,
        ..RootRequest::default()
    };
    let page = hub.root_inbox(&world.root, req).unwrap();
    assert_eq!(texts(&page), ["new"]);
}

#[test]
fn the_rules_are_re_run_over_the_stored_text_and_a_block_is_withheld_not_returned() {
    for (store, _, _guard) in stores() {
        let world = World::new(store);
        let a = world.seat("a");
        let events = Events::new();
        let hub = world
            .hub(Recorder::deliver())
            .with_quarantine_sink(events.clone());
        let blocked = put_mail(&world, &a, BLOCKED);
        let clean = put_mail(&world, &a, CLEAN);
        let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert_eq!(texts(&page), [CLEAN]);
        assert_eq!((page.withheld, page.remaining), (1, 0));
        assert_eq!(state_of(&world, blocked), DeliveryState::Expired);
        assert_eq!(state_of(&world, clean), DeliveryState::Delivered);
        assert!(events.all().is_empty());
        let shown = format!("{page:?}{}", json_of(&page));
        assert!(!shown.contains(BLOCKED));
        let copies = world.everything_from(&a);
        let withheld = copies.iter().filter(|m| m.state == DeliveryState::Withheld);
        assert_eq!(withheld.count(), 1);
        let again = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert_eq!((again.withheld, again.messages.len()), (0, 1));
    }
}

#[test]
fn a_rules_quarantine_is_held_reported_once_and_never_returned() {
    for (store, _, _guard) in stores() {
        let world = World::new(store);
        let a = world.seat("a");
        let events = Events::new();
        let hub = world
            .hub(Recorder::deliver())
            .with_quarantine_sink(events.clone());
        let original = put_mail(&world, &a, CONTENT);
        let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert!(page.messages.is_empty());
        assert_eq!((page.withheld, page.remaining), (1, 0));
        assert_eq!(state_of(&world, original), DeliveryState::Expired);
        let seen = events.all();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].origin, QuarantineOrigin::Recv);
        assert_eq!(seen[0].codes, [ReasonCode::SecretLeak]);
        assert_eq!(seen[0].from, *a.session());
        let held = world.messages.quarantined().unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].held, [ReasonCode::SecretLeak]);
        assert_eq!(held[0].to, mailbox_of(&world.root));
        hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert_eq!(events.all().len(), 1);
    }
}

#[test]
fn a_failing_sink_still_withholds_the_message() {
    let world = World::memory();
    let a = world.seat("a");
    let events = Events::failing();
    let hub = world
        .hub(Recorder::deliver())
        .with_quarantine_sink(events.clone());
    put_mail(&world, &a, CONTENT);
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    assert_eq!((page.messages.len(), page.withheld), (0, 1));
    assert_eq!(events.all().len(), 1);
}

#[test]
fn a_verdict_that_no_longer_binds_the_text_quarantines_it() {
    for (store, _, _guard) in stores() {
        let world = World::new(store);
        let a = world.seat("a");
        let events = Events::new();
        let hub = world
            .hub(Recorder::deliver())
            .with_quarantine_sink(events.clone());
        let mut tampered = mail(&a, "status", CLEAN, DeliveryState::Pending);
        tampered.outbound_verdict.content_hash ^= 1;
        let original = tampered.id;
        world.messages.append(tampered).unwrap();
        let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
        assert!(page.messages.is_empty());
        assert_eq!((page.withheld, page.remaining), (1, 0));
        assert_eq!(state_of(&world, original), DeliveryState::Expired);
        assert_eq!(events.all().len(), 1);
        assert_eq!(events.all()[0].codes, [ReasonCode::Other]);
        let held = world.messages.quarantined().unwrap();
        assert_eq!(held.len(), 1);
    }
}

fn quarantined_with_mismatch(a: &SessionBinding) -> StoredMessage {
    let mut message = mail(
        a,
        "status",
        "withheld secret_leak 9 bytes",
        DeliveryState::Quarantined,
    );
    message.outbound_verdict.content_hash ^= 1;
    message.held = vec![ReasonCode::SecretLeak];
    message
}

fn kept_copy(world: &World, a: &SessionBinding) -> StoredMessage {
    let message = quarantined_with_mismatch(a);
    let id = message.id;
    world.messages.append(message).unwrap();
    world.messages.get(id).unwrap().unwrap()
}

fn variant(message: &StoredMessage, state: &str, held: bool, marked: bool) -> StoredMessage {
    let mut json = serde_json::to_value(message).unwrap();
    json["state"] = serde_json::json!(state);
    if !held {
        json["held"] = serde_json::json!([]);
    }
    if marked {
        json["released"] = serde_json::json!({"at": "2023-11-14T22:13:20Z"});
    }
    serde_json::from_value(json).unwrap()
}

#[test]
fn a_release_marker_is_honoured_only_on_a_pending_held_message() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let a = world.seat("a");
    let events = Events::new();
    let hub = world
        .hub(Recorder::deliver())
        .with_quarantine_sink(events.clone());
    let no_hold = variant(&kept_copy(&world, &a), "pending", false, true);
    let delivered = variant(&kept_copy(&world, &a), "delivered", true, true);
    let unmarked = variant(&kept_copy(&world, &a), "pending", true, false);
    let valid = variant(&kept_copy(&world, &a), "pending", true, true);
    *flaky.lie.lock().unwrap() = vec![no_hold, delivered, unmarked, valid];
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    assert_eq!(texts(&page), ["withheld secret_leak 9 bytes"]);
    assert!(page.messages[0].released_by_human);
    assert!(json_of(&page).contains("\"released_by_human\":true"));
    assert_eq!((page.withheld, page.remaining), (3, 0));
    assert_eq!(events.all().len(), 3);
}

#[test]
fn identity_subject_and_body_are_fenced_like_recv_and_carry_no_raw_control_text() {
    let world = World::memory();
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let hostile = "evil\u{1b}[31m\u{202e}\u{85}name";
    let mut to_root = mail(&a, "a subject", CLEAN, DeliveryState::Pending);
    to_root.from_identity = hostile.to_string();
    world.messages.append(to_root).unwrap();
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    let shown = &page.messages[0];
    assert!(shown.body.starts_with(FenceLabel::AgentMessage.open()));
    assert!(shown.body.ends_with(FenceLabel::AgentMessage.close()));
    assert!(shown.subject.starts_with(FenceLabel::AgentMessage.open()));
    assert!(shown
        .from_identity
        .starts_with(FenceLabel::AgentText.open()));
    let raw = json_of(&page);
    let bad = |c: char| c == '\u{1b}' || c == '\u{202e}' || c == '\u{85}';
    assert!(!shown.from_identity.contains(bad));
    assert!(!raw.contains(bad));
    let caps = HubLimits::default().body_max;
    let identity = fence_of(FenceLabel::AgentText, hostile, IDENTITY_RENDER_CAP);
    let subject = fence_of(FenceLabel::AgentMessage, "a subject", SUBJECT_RENDER_CAP);
    let body = fence_of(FenceLabel::AgentMessage, CLEAN, caps);
    assert_eq!(shown.from_identity, identity);
    assert_eq!(shown.subject, subject);
    assert_eq!(shown.body, body);
}

#[test]
fn debug_prints_sizes_and_never_text() {
    let world = World::memory();
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    put_mail(&world, &a, "plant-this-text-please");
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    let shown = format!("{page:?}");
    assert!(!shown.contains("plant-this-text"));
    assert!(shown.contains("body_bytes"));
}

#[test]
fn the_request_clamps_to_one_through_twenty_and_reports_what_remains() {
    let world = World::memory();
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    for n in 0..25 {
        put_mail(&world, &a, &format!("note {n}"));
    }
    let one = hub.root_inbox(&world.root, ask(0)).unwrap();
    assert_eq!((one.messages.len(), one.remaining), (1, 24));
    let most = hub.root_inbox(&world.root, ask(255)).unwrap();
    assert_eq!((most.messages.len(), most.remaining), (20, 5));
}

#[test]
fn a_page_stops_at_the_byte_cap_but_always_returns_the_first_message() {
    let world = World::memory();
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let long = "ok ".repeat(2600);
    for _ in 0..5 {
        put_mail(&world, &a, &long);
    }
    let page = hub.root_inbox(&world.root, ask(20)).unwrap();
    assert_eq!((page.messages.len(), page.remaining), (4, 1));
}

#[test]
fn store_failures_are_errors_and_deliver_nothing() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let id = put_mail(&world, &a, CLEAN);
    flaky.fail_inbox.store(true, Ordering::SeqCst);
    assert!(hub.root_inbox(&world.root, RootRequest::default()).is_err());
    flaky.fail_inbox.store(false, Ordering::SeqCst);
    flaky.fail_ack.store(true, Ordering::SeqCst);
    let acking = RootRequest {
        ack_through: Some(Seq::new(9)),
        ..RootRequest::default()
    };
    assert!(hub.root_inbox(&world.root, acking).is_err());
    flaky.fail_ack.store(false, Ordering::SeqCst);
    flaky.fail_set_state_at.store(1, Ordering::SeqCst);
    assert!(hub.root_inbox(&world.root, RootRequest::default()).is_err());
    assert_eq!(state_of(&world, id), DeliveryState::Pending);
}

#[test]
fn a_failure_after_the_first_message_returns_what_was_delivered() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let first = put_mail(&world, &a, "one");
    let second = put_mail(&world, &a, "two");
    flaky.fail_set_state_at.store(2, Ordering::SeqCst);
    let page = hub.root_inbox(&world.root, RootRequest::default()).unwrap();
    assert_eq!(texts(&page), ["one"]);
    assert_eq!(page.remaining, 1);
    assert_eq!(state_of(&world, first), DeliveryState::Delivered);
    assert_eq!(state_of(&world, second), DeliveryState::Pending);
}

#[test]
fn a_retire_failure_with_nothing_shown_is_an_error() {
    let flaky = Flaky::new();
    let world = World::new(flaky.clone());
    let a = world.seat("a");
    let hub = world.hub(Recorder::deliver());
    let id = put_mail(&world, &a, BLOCKED);
    flaky.fail_append.store(true, Ordering::SeqCst);
    assert!(hub.root_inbox(&world.root, RootRequest::default()).is_err());
    assert_eq!(state_of(&world, id), DeliveryState::Pending);
}

fn production_source() -> String {
    let source = include_str!("../root_inbox.rs").replace("\r\n", "\n");
    let code: Vec<&str> = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("///"))
        .collect();
    code.join("\n")
}

#[test]
fn the_root_inbox_source_has_no_panic_paths_no_indexing_no_logging_and_no_marker_writes() {
    let source = production_source();
    for banned in [
        ".unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "tracing::",
        "transition(",
        "Release {",
        "released:",
        "review_inbound",
        "auditors",
    ] {
        assert!(!source.contains(banned), "{banned}");
    }
    let chars: Vec<char> = source.chars().collect();
    for pair in chars.windows(2) {
        let indexing =
            (pair[0].is_alphanumeric() || pair[0] == '_' || pair[0] == ')') && pair[1] == '[';
        assert!(!indexing, "indexing near {pair:?}");
    }
}

#[test]
fn the_root_inbox_acks_and_writes_state_only_through_the_mailbox_and_the_show_path() {
    let source = production_source();
    let acks: Vec<&str> = source.matches(".ack(").collect();
    assert_eq!(acks.len(), 1);
    assert!(source.contains(".ack(mailbox.session().clone(), upto)"));
    assert_eq!(source.matches(".set_state(").count(), 1);
    assert_eq!(source.matches(".retire(").count(), 1);
    assert_eq!(source.matches("MailboxId::for_root(root)").count(), 1);
    assert_eq!(source.matches("self.messages.inbox(").count(), 1);
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Clean,
    Blocked,
    Tampered,
}

fn kinds() -> impl Strategy<Value = Kind> {
    prop_oneof![Just(Kind::Clean), Just(Kind::Blocked), Just(Kind::Tampered)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn pages_concatenate_to_the_roots_deliverable_mail_in_order_without_duplicates_or_leaks(
        plan in proptest::collection::vec((0usize..2, kinds()), 0..30),
        sizes in proptest::collection::vec(1u8..=20, 1..6),
    ) {
        let world = World::memory();
        let roots = [world.root.clone(), other_root()];
        let senders = [
            world.seat_in(&roots[0], "a", EffectClass::None),
            world.seat_in(&roots[1], "x", EffectClass::None),
        ];
        let hub = world.hub(Recorder::deliver());
        let mut model: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for (n, (side, kind)) in plan.iter().enumerate() {
            let body = match kind {
                Kind::Clean => format!("note number {n}"),
                Kind::Blocked => format!("token=abc{n}"),
                Kind::Tampered => format!("tampered number {n}"),
            };
            let mut message = mail(&senders[*side], "status", &body, DeliveryState::Pending);
            if matches!(kind, Kind::Tampered) {
                message.outbound_verdict.content_hash ^= 1;
            }
            if matches!(kind, Kind::Clean) {
                model[*side].push(body);
            }
            world.messages.append(message).unwrap();
        }
        for side in 0..2 {
            let mut seen: Vec<String> = Vec::new();
            let mut after: Option<Seq> = None;
            for turn in 0..200usize {
                let max = sizes[turn % sizes.len()];
                let req = RootRequest { max, after, ack_through: None };
                let page = hub.root_inbox(&roots[side], req).unwrap();
                for message in &page.messages {
                    seen.push(inner_text(&message.body).to_string());
                }
                if let Some(last) = page.messages.last() {
                    after = Some(last.seq);
                }
                if page.messages.is_empty() && page.remaining == 0 {
                    break;
                }
            }
            prop_assert_eq!(&seen, &model[side]);
        }
    }
}
