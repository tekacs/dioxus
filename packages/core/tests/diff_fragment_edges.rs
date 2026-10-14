use dioxus::prelude::*;
use dioxus_core::generation;
use dioxus_renderer_oracle::RendererOracle;

/// A sibling has already committed its nested fragment replacement when the
/// next empty sibling asks for an insertion edge in the outer fragment.
#[test]
fn nested_fragment_replacement_before_empty_sibling_grows() {
    fn app() -> Element {
        let changed = generation() % 2 == 1;
        let nested = if changed {
            VNode::empty()
        } else {
            rsx! { span { "old" } }
        };
        let children = [
            rsx! { {nested} },
            rsx! { if changed { "new" } },
        ];
        rsx! { div { {children.into_iter()} } }
    }

    fn initial() -> Element {
        rsx! { div { span { "old" } } }
    }

    fn changed() -> Element {
        rsx! { div { "new" } }
    }

    let mut dom = VirtualDom::new(app);
    let mut oracle = RendererOracle::new();
    oracle.rebuild(&mut dom);
    oracle.assert_matches(initial);

    for expected in [changed, initial, changed, initial] {
        dom.mark_dirty(ScopeId::APP);
        oracle.render(&mut dom);
        oracle.assert_matches(expected);
        oracle.assert_stack_clean();
    }
}
