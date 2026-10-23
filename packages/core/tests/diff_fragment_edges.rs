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
        let children = [rsx! { {nested} }, rsx! { if changed { "new" } }];
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

/// Removing a nested conditional commits earlier fragment children before a
/// following component with changed props resolves its own insertion site.
#[test]
fn recursive_fragment_edges_after_conditional_component_removal() {
    fn app() -> Element {
        let changed = generation() % 2 == 1;
        rsx! {
            Panel {
                if !changed {
                    span { "head" }
                    if !changed { Label { text: "nested" } }
                    if !changed { Label { text: "tail" } }
                }
                if true { Label { text: if changed { "new" } else { "old" } } }
            }
        }
    }

    fn initial() -> Element {
        rsx! { div { span { "head" } "nested" "tail" "old" } }
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

#[component]
fn Panel(children: Element) -> Element {
    rsx! { div { {children} } }
}

#[component]
fn Label(text: String) -> Element {
    rsx! { if !text.is_empty() { "{text}" } }
}

#[test]
fn unkeyed_nested_fragment_edge_matrix() {
    fn app() -> Element {
        scene(generation(), false)
    }
    assert_scene(app);
}

#[test]
fn keyed_nested_fragment_edge_matrix() {
    fn app() -> Element {
        scene(generation(), true)
    }
    assert_scene(app);
}

fn assert_scene(app: fn() -> Element) {
    let mut dom = VirtualDom::new(app);
    let mut oracle = RendererOracle::new();
    oracle.rebuild(&mut dom);
    oracle.assert_matches_vdom(&dom);

    // Repeat prepend, reorder, truncate, clear and repopulate while nested
    // siblings independently replace static, text, multi-root and empty output.
    for _ in 0..36 {
        dom.mark_dirty(ScopeId::APP);
        oracle.render(&mut dom);
        oracle.assert_matches_vdom(&dom);
        oracle.assert_stack_clean();
    }
}

fn scene(phase: usize, keyed: bool) -> Element {
    let order: &[usize] = match phase % 6 {
        0 => &[0, 1, 2],
        1 => &[3, 0, 1, 2],
        2 => &[2, 0, 3, 1],
        3 => &[0, 2],
        4 => &[],
        _ => &[4, 0, 1, 2, 3],
    };
    let children = order.iter().map(|&id| {
        let nested = [content(phase + id, id), content(phase * 2 + id + 1, id)];
        let nested = rsx! {
            {nested.into_iter().map(|node| rsx! { {node} })}
        };
        if keyed {
            rsx! { Fragment { key: "{id}", {nested} } }
        } else {
            nested
        }
    });
    rsx! { div { "before" {children} "after" } }
}

fn content(shape: usize, id: usize) -> Element {
    match shape % 5 {
        0 => VNode::empty(),
        1 => rsx! { span { "{id}" } },
        2 => rsx! { "text-{id}" },
        3 => rsx! { em { "{id}" } strong { "tail" } },
        _ => rsx! { if id % 2 == 0 { "even" } },
    }
}
