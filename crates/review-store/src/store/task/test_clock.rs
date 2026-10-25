//! Scoped Store unit-test time. No environment or non-test build can enable this seam.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static TIME: RefCell<Option<Rc<Cell<u64>>>> = const { RefCell::new(None) };
}

pub(super) fn now() -> Option<u64> {
    TIME.with(|slot| slot.borrow().as_ref().map(|clock| clock.get()))
}

/// Override only this thread, restoring its previous clock even during unwinding.
/// The closure bounds the guard's lifetime and enforces nested restoration order.
pub(super) fn with_time<T>(time: u64, run: impl FnOnce(&Cell<u64>) -> T) -> T {
    struct Restore(Option<Rc<Cell<u64>>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TIME.with(|slot| slot.replace(self.0.take()));
        }
    }

    let clock = Rc::new(Cell::new(time));
    let _restore = Restore(TIME.with(|slot| slot.replace(Some(clock.clone()))));
    run(&clock)
}

#[test]
fn scoped_clock_restores_after_nesting_and_unwinding() {
    assert_eq!(now(), None);
    with_time(100, |outer| {
        assert_eq!(super::now().unwrap(), 100);
        with_time(200, |inner| {
            inner.set(201);
            assert_eq!(super::now().unwrap(), 201);
            // Updating the suspended outer clock cannot alter the inner clock.
            outer.set(101);
            assert_eq!(super::now().unwrap(), 201);
        });
        assert_eq!(super::now().unwrap(), 101);
        let panic = std::panic::catch_unwind(|| {
            with_time(300, |_| {
                assert_eq!(super::now().unwrap(), 300);
                panic!("exercise clock restoration");
            });
        });
        assert!(panic.is_err());
        assert_eq!(super::now().unwrap(), 101);
    });
    assert_eq!(now(), None);
    assert!(std::panic::catch_unwind(|| with_time(400, |_| panic!("outer unwind"))).is_err());
    assert_eq!(now(), None);
    // With no override, the Store returns to its existing clock path.
    assert!(super::now().is_ok());
}

#[test]
fn scoped_clocks_are_independent_on_concurrent_threads_and_not_inherited() {
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        with_time(100, |parent| {
            let children: Vec<_> = [200, 300]
                .into_iter()
                .map(|time| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let inherited = now();
                        let readings = with_time(time, |clock| {
                            barrier.wait();
                            let first = super::now().unwrap();
                            clock.set(time + 1);
                            barrier.wait();
                            (first, super::now().unwrap())
                        });
                        (inherited, readings, now())
                    })
                })
                .collect();
            barrier.wait();
            let first = super::now().unwrap();
            parent.set(101);
            barrier.wait();
            assert_eq!((first, super::now().unwrap()), (100, 101));
            for (child, time) in children.into_iter().zip([200, 300]) {
                assert_eq!(child.join().unwrap(), (None, (time, time + 1), None));
            }
        });
    });
    assert_eq!(now(), None);
}
