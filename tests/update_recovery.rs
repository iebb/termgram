use grammers_client::tl;
use grammers_session::{
    types::{ChannelState, UpdatesState},
    updates::{MessageBoxes, UpdatesLike},
};

#[test]
fn startup_recovers_common_state_without_scanning_every_channel() {
    let state = UpdatesState {
        pts: 12,
        qts: 0,
        date: 123,
        seq: 1,
        channels: (1..=500).map(|id| ChannelState { id, pts: 20 }).collect(),
    };
    let boxes = MessageBoxes::load(state);
    assert!(boxes.get_difference().is_some());
    assert!(boxes.get_channel_difference().is_none());
    assert_eq!(boxes.session_state().channels.len(), 500);
}

#[test]
fn channel_gap_recovers_from_the_local_cursor() {
    let mut boxes = MessageBoxes::new();
    boxes.try_set_channel_state(42, 20);
    boxes
        .process_updates(UpdatesLike::Updates(tl::enums::Updates::UpdateShort(
            tl::types::UpdateShort {
                update: tl::types::UpdateChannelTooLong {
                    channel_id: 42,
                    pts: Some(30),
                }
                .into(),
                date: 123,
            },
        )))
        .unwrap();
    let request = boxes
        .get_channel_difference()
        .expect("channel gap must trigger recovery");
    assert_eq!(request.pts, 20);
}

#[tokio::test]
async fn sdk_delivers_the_whole_batch_before_its_checkpoint() {
    use grammers_client::{Client, SenderPool, client::UpdatesConfiguration};
    use grammers_session::storages::MemorySession;
    use std::sync::Arc;
    let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
    let client = Client::new(pool.handle);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut stream = client
        .stream_updates(
            receiver,
            UpdatesConfiguration {
                catch_up: false,
                update_queue_limit: None,
            },
        )
        .await
        .unwrap();
    sender
        .send(UpdatesLike::Updates(
            tl::types::Updates {
                updates: vec![tl::enums::Update::PtsChanged, tl::enums::Update::PtsChanged],
                users: Vec::new(),
                chats: Vec::new(),
                date: 123,
                seq: 1,
            }
            .into(),
        ))
        .unwrap();
    let (updates, cursor) = Box::pin(stream.next_batch()).await.unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(cursor.seq, 1);
    assert_eq!(cursor.date, 123);

    // Own delete RPCs use UpdateShort with NO_DATE and must carry their IDs in
    // the very batch that advances the durable cursor, just like server pushes.
    sender
        .send(UpdatesLike::Updates(
            tl::types::UpdateShort {
                update: tl::types::UpdateDeleteMessages {
                    messages: vec![41, 42],
                    pts: 2,
                    pts_count: 2,
                }
                .into(),
                date: 0,
            }
            .into(),
        ))
        .unwrap();
    let (updates, cursor) = Box::pin(stream.next_batch()).await.unwrap();
    let [grammers_client::update::Update::MessageDeleted(deleted)] = updates.as_slice() else {
        panic!("deletion IDs must arrive with their checkpoint")
    };
    assert_eq!(deleted.messages(), [41, 42]);
    assert_eq!(cursor.pts, 2);
    assert_eq!(cursor.date, 123, "NO_DATE retains the previous date");

    sender
        .send(UpdatesLike::Updates(
            tl::types::UpdateShort {
                update: tl::types::UpdateReadMessagesContents {
                    messages: vec![43],
                    pts: 3,
                    pts_count: 1,
                    date: None,
                }
                .into(),
                date: 0,
            }
            .into(),
        ))
        .unwrap();
    let (updates, cursor) = Box::pin(stream.next_batch()).await.unwrap();
    let [grammers_client::update::Update::Raw(raw)] = updates.as_slice() else {
        panic!("content receipt must arrive before its checkpoint")
    };
    let tl::enums::Update::ReadMessagesContents(read) = &raw.raw else {
        panic!("content receipt")
    };
    assert_eq!(read.messages, [43]);
    assert_eq!(cursor.pts, 3);
    assert_eq!(cursor.date, 123);
}

#[test]
fn active_group_uses_server_timeout_and_stops_when_closed() {
    let mut boxes = MessageBoxes::new();
    boxes.try_set_channel_state(42, 20);
    boxes.set_active_channel(Some((42, 99)));
    assert_eq!(boxes.get_channel_difference().unwrap().pts, 20);
    boxes.apply_channel_difference(
        tl::types::updates::ChannelDifferenceEmpty {
            r#final: true,
            pts: 21,
            timeout: Some(3),
        }
        .into(),
    );
    assert!(boxes.get_channel_difference().is_none());
    let delay = boxes
        .check_deadlines()
        .duration_since(std::time::Instant::now());
    assert!(delay.as_secs() <= 3);
    assert!(delay.as_secs() >= 2);
    boxes.set_active_channel(None);
    let delay = boxes
        .check_deadlines()
        .duration_since(std::time::Instant::now());
    assert!(
        delay.as_secs() > 60,
        "closing the group stops short polling"
    );
    assert!(boxes.get_channel_difference().is_none());
}

#[test]
fn viewed_channel_recovers_first_and_is_not_reported_as_backlog() {
    let too_long = |channel_id| -> tl::enums::Update {
        tl::types::UpdateChannelTooLong {
            channel_id,
            pts: Some(30),
        }
        .into()
    };
    let mut boxes = MessageBoxes::new();
    for id in [7, 42] {
        boxes.try_set_channel_state(id, 20);
    }
    assert_eq!(boxes.recovery(), (false, 0));
    boxes
        .process_updates(UpdatesLike::Updates(
            tl::types::Updates {
                updates: vec![too_long(7), too_long(42)],
                users: Vec::new(),
                chats: Vec::new(),
                date: 123,
                seq: 0,
            }
            .into(),
        ))
        .unwrap();
    assert_eq!(boxes.recovery(), (false, 2));
    boxes.set_active_channel(Some((42, 20)));
    assert_eq!(boxes.recovery(), (false, 1));
    let tl::enums::InputChannel::Channel(first) = boxes.get_channel_difference().unwrap().channel
    else {
        panic!("expected a channel request");
    };
    assert_eq!(first.channel_id, 42);
}
