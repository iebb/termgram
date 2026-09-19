//! Separate OS processes exercise SQLite locking and simultaneous first-open.
use std::process::{Command, Stdio};

use grammers_session::{Session, storages::SqliteSession, types::UpdateState};

#[test]
fn concurrent_session_processes() {
    let directory = std::env::temp_dir().join(format!("termgram-session-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("shared.session");
    let mut children = (0..4)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "session_writer", "--nocapture"])
                .env("TERMGRAM_TEST_SESSION", &path)
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let success = children
        .iter_mut()
        .all(|child| child.wait().unwrap().success());
    // Reap every child even if an earlier writer failed.
    for child in &mut children {
        let _ = child.wait();
    }
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        success,
        "all processes must initialize and update the shared session"
    );
}

#[test]
fn session_writer() {
    let Some(path) = std::env::var_os("TERMGRAM_TEST_SESSION") else {
        return;
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let session = SqliteSession::open(path).await.unwrap();
            // Exercise initialization and repeated writes, not sustained writer
            // saturation: the production busy timeout deliberately caps waiting.
            for pts in 1..=3 {
                session
                    .set_update_state(UpdateState::Primary {
                        pts,
                        date: pts,
                        seq: pts,
                    })
                    .await
                    .unwrap();
                session.set_home_dc_id(2).await.unwrap();
                assert!(session.updates_state().await.unwrap().pts > 0);
            }
        });
}
