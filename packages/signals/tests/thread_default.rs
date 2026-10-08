//! A thread's default application serves code running outside any runtime.

use dioxus::prelude::*;
use dioxus_core::Runtime;
use std::panic::{AssertUnwindSafe, catch_unwind};

static COUNT: GlobalSignal<i32> = Signal::global(|| 0);

fn app() -> VirtualDom {
    let mut dom = VirtualDom::new(|| rsx! {});
    dom.rebuild_in_place();
    dom
}

fn panics(f: impl FnOnce()) -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    result.is_err()
}

#[test]
fn globals_resolve_outside_any_runtime() {
    let dom = app();
    assert!(panics(|| *COUNT.write() = 1), "no default yet");

    Runtime::set_thread_default(&dom.runtime());
    *COUNT.write() = 2;
    assert_eq!(*COUNT.read(), 2);
    assert_eq!(
        dom.in_runtime(|| *COUNT.peek()),
        2,
        "the default's own copy"
    );
}

#[test]
fn an_entered_runtime_wins_over_the_default() {
    let default = app();
    let entered = app();
    Runtime::set_thread_default(&default.runtime());

    entered.in_runtime(|| *COUNT.write() = 7);
    assert_eq!(*COUNT.read(), 0, "the default application is unchanged");
    assert_eq!(entered.in_runtime(|| *COUNT.peek()), 7);
}

#[test]
fn a_dropped_default_is_not_retained() {
    let dom = app();
    let weak = std::rc::Rc::downgrade(&dom.runtime());
    Runtime::set_thread_default(&dom.runtime());
    drop(dom);

    assert!(weak.upgrade().is_none());
    assert!(Runtime::try_current().is_none());
}

#[test]
fn the_default_supplies_no_scope() {
    let dom = app();
    Runtime::set_thread_default(&dom.runtime());

    assert!(
        panics(|| {
            Signal::new(0);
        }),
        "creating owned state still requires an entered scope"
    );
}
