use super::*;
#[path = "../../support/captured_review_recovery.rs"]
mod fixture;

#[test]
fn expired_waiting_recovers_selected_review_without_new_work_or_resources() {
    fixture::run_expired_waiting(|cas, store, limits| {
        admit_with_limits(cas, store, &command_pipeline(), limits)
    });
}
