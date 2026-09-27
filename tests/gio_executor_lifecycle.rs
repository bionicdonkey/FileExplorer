use std::sync::Arc;

use zorin_explorer::infrastructure::gio_executor::GioExecutor;

#[test]
fn shared_executor_is_process_wide_and_joins_when_no_adapter_owns_it() {
    let first = GioExecutor::shared();
    let first_weak = Arc::downgrade(&first);
    drop(first);

    let second = GioExecutor::shared();
    assert!(first_weak
        .upgrade()
        .is_some_and(|first| Arc::ptr_eq(&first, &second)));

    drop(second);
    assert!(GioExecutor::shutdown_shared_if_idle_for_tests());
    assert!(first_weak.upgrade().is_none());
}
