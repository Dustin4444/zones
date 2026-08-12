//! Runtime queue and retry-budget tests.

use super::*;

#[test]
fn bounded_queue_keeps_one_current_and_preserves_order() {
    let (_directory, store) = create();
    let mut runtime = Runtime::new(
        store.load().unwrap(),
        2,
        RetryBudget::new(1, Duration::ZERO),
    );
    assert_eq!(runtime.state(), RuntimeState::Starting);
    let plan = NotificationPlan {
        reverted: vec![],
        ancestor: BlockNumHash {
            number: 0,
            hash: B256::ZERO,
        },
        applied: vec![BlockNumHash {
            number: 1,
            hash: B256::repeat_byte(1),
        }],
        acknowledge: BlockNumHash {
            number: 1,
            hash: B256::repeat_byte(1),
        },
    };
    assert!(runtime.enqueue(1, plan.clone()).is_ok());
    assert!(runtime.enqueue(2, plan.clone()).is_ok());
    assert!(runtime.enqueue(3, plan.clone()).is_ok());
    assert_eq!(runtime.enqueue(4, plan), Err(4));
    runtime.advance();
    assert_eq!(
        runtime.current().map(|(notification, _)| notification),
        Some(&2)
    );
    runtime.advance();
    assert_eq!(
        runtime.current().map(|(notification, _)| notification),
        Some(&3)
    );
}

#[test]
fn attempt_and_elapsed_budgets_are_both_terminal() {
    let attempts = RetryBudget::new(2, Duration::from_secs(60));
    let now = Instant::now();
    assert!(!attempts.exhausted(1, now, now));
    assert!(attempts.exhausted(2, now, now));
    let elapsed = RetryBudget::new(99, Duration::ZERO);
    assert!(elapsed.exhausted(0, now, now));
}
