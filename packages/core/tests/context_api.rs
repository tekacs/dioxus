use dioxus::prelude::*;
use dioxus_core::{ScopeId, consume_context_from_scope, current_scope_id, generation};
use dioxus_renderer_oracle::RendererOracle;
use std::cell::Cell;

#[test]
fn scope_identity_survives_keyed_reordering() {
    check_identity(false);
}

#[test]
fn keyed_suspense_keeps_scope_identity() {
    check_identity(true);
}

fn check_identity(boundary: bool) {
    use std::{cell::RefCell, rc::Rc};
    type Seen = Rc<RefCell<Vec<(&'static str, ScopeId, ScopeId, Rc<()>)>>>;

    fn app(boundary: bool) -> Element {
        let cycle = generation();
        let order = if cycle == 0 {
            ["left", "right"]
        } else {
            ["right", "left"]
        };
        if boundary {
            rsx! {
                for key in order {
                    SuspenseBoundary {
                        key: "boundary:{key}",
                        fallback: |_| rsx! {},
                        Child { key: "{key}", label: key, cycle, boundary }
                    }
                }
            }
        } else {
            rsx! {
                for key in order {
                    Child { key: "{key}", label: key, cycle, boundary }
                }
            }
        }
    }

    #[component]
    fn Child(label: &'static str, cycle: usize, boundary: bool) -> Element {
        assert!(current_scope_name().ends_with("Child"));
        assert_eq!(current_scope_key().as_deref(), Some(label));
        let parent = dioxus_core::parent_scope().unwrap();
        if boundary {
            let (name, key) = dioxus_core::Runtime::current()
                .in_scope(parent, || (current_scope_name(), current_scope_key()));
            assert!(name.ends_with("SuspenseBoundary"));
            assert_eq!(key.as_deref(), Some(format!("boundary:{label}").as_str()));
        }
        let state = use_hook(|| Rc::new(()));
        consume_context::<Seen>()
            .borrow_mut()
            .push((label, current_scope_id(), parent, state));
        rsx!("{label}:{cycle}")
    }

    let seen = Seen::default();
    let mut dom = VirtualDom::new_with_props(app, boundary).with_root_context(seen.clone());
    let mut oracle = RendererOracle::new();
    oracle.rebuild(&mut dom);
    oracle.assert_matches(|| rsx!("left:0" "right:0"));
    let original = seen.borrow().clone();
    assert_eq!(original.len(), 2);
    assert_ne!(original[0].1, original[1].1);

    let expected: [fn() -> Element; 2] = [|| rsx!("right:1" "left:1"), || rsx!("right:2" "left:2")];
    for expected in expected {
        seen.borrow_mut().clear();
        dom.mark_dirty(ScopeId::APP);
        oracle.render(&mut dom);
        oracle.assert_matches(expected);
        // Changed props force both children to render; matching the hook allocation
        // also rejects a remount that happens to recycle the same ScopeId.
        assert_eq!(seen.borrow().len(), 2);
        for (label, id, parent, state) in seen.borrow().iter() {
            let previous = original.iter().find(|entry| entry.0 == *label).unwrap();
            assert_eq!(*id, previous.1);
            assert_eq!(*parent, previous.2);
            assert!(Rc::ptr_eq(state, &previous.3));
        }
    }
}

#[test]
fn state_shares() {
    thread_local! {
        static CHILD_2_SCOPE: Cell<Option<ScopeId>> = const { Cell::new(None) };
    }

    fn app() -> Element {
        provide_context(generation() as i32);

        rsx!(child_1 {})
    }

    fn child_1() -> Element {
        rsx!(child_2 {})
    }

    fn child_2() -> Element {
        CHILD_2_SCOPE.with(|scope| scope.set(Some(current_scope_id())));
        let value = consume_context::<i32>();
        rsx!("Value is {value}")
    }

    fn expected_0() -> Element {
        rsx!("Value is 0")
    }

    fn expected_2() -> Element {
        rsx!("Value is 2")
    }

    fn expected_3() -> Element {
        rsx!("Value is 3")
    }

    let mut dom = VirtualDom::new(app);
    let mut oracle = RendererOracle::new();
    CHILD_2_SCOPE.with(|scope| scope.set(None));
    oracle.rebuild(&mut dom);
    let child_2_scope =
        CHILD_2_SCOPE.with(|scope| scope.get().expect("child_2 should have rendered"));
    oracle.assert_matches(expected_0);

    dom.mark_dirty(ScopeId::APP);
    oracle.render(&mut dom);
    dom.in_runtime(|| {
        assert_eq!(consume_context_from_scope::<i32>(ScopeId::APP).unwrap(), 1);
    });

    dom.mark_dirty(ScopeId::APP);
    oracle.render(&mut dom);
    dom.in_runtime(|| {
        assert_eq!(consume_context_from_scope::<i32>(ScopeId::APP).unwrap(), 2);
    });

    dom.mark_dirty(child_2_scope);
    let summary = oracle.render(&mut dom);
    oracle.assert_matches(expected_2);
    assert_eq!(summary.set_texts, 1);

    dom.mark_dirty(ScopeId::APP);
    dom.mark_dirty(child_2_scope);
    let summary = oracle.render(&mut dom);
    oracle.assert_matches(expected_3);
    assert_eq!(summary.set_texts, 1);
}
