use corescope_lib::inventory::{ScanAbort, ScanCoordinator};
use std::time::Duration;

#[tokio::test]
async fn user_cancellation_interrupts_the_active_scan_and_releases_its_slot() {
    let coordinator = ScanCoordinator::default();
    let (context, completion) = coordinator
        .begin_with_timeout(Duration::from_secs(5))
        .expect("first scan should start");
    let scan = tokio::spawn(async move {
        context
            .race(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
            })
            .await
    });

    tokio::task::yield_now().await;
    coordinator.cancel().expect("active scan should cancel");

    assert_eq!(
        scan.await.expect("scan task should finish"),
        Err(ScanAbort::Cancelled)
    );
    drop(completion);
    assert!(
        coordinator.begin().is_ok(),
        "a completed scan should release the slot"
    );
}

#[tokio::test]
async fn scan_timeout_interrupts_the_operation_and_releases_its_slot() {
    let coordinator = ScanCoordinator::default();
    let (context, completion) = coordinator
        .begin_with_timeout(Duration::from_millis(15))
        .expect("scan should start");

    let result = context
        .race(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;

    assert_eq!(result, Err(ScanAbort::TimedOut));
    drop(completion);
    assert!(
        coordinator.begin().is_ok(),
        "a timed out scan should release the slot"
    );
}

#[test]
fn concurrent_scans_are_rejected_instead_of_overlapping_hardware_reads() {
    let coordinator = ScanCoordinator::default();
    let (_context, _completion) = coordinator.begin().expect("first scan should start");

    let error = match coordinator.begin() {
        Err(error) => error,
        Ok(_) => panic!("second scan must not overlap the active scan"),
    };

    assert_eq!(error, "SCAN_ALREADY_RUNNING");
}

#[test]
fn scan_slot_stays_reserved_until_the_worker_and_command_are_both_done() {
    let coordinator = ScanCoordinator::default();
    let (_context, command_completion) = coordinator.begin().expect("scan should start");
    let worker_completion = command_completion.clone();

    drop(command_completion);
    assert_eq!(
        coordinator.begin().err().as_deref(),
        Some("SCAN_ALREADY_RUNNING"),
        "worker cleanup must finish before another scan can start"
    );

    drop(worker_completion);
    assert!(
        coordinator.begin().is_ok(),
        "the last completion lease should release the slot"
    );
}
