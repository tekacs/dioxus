use dioxus_stores::*;
use std::collections::HashMap;

fn run_with_runtime<T>(f: impl FnOnce() -> T) -> T {
    use dioxus::prelude::*;

    fn app() -> Element {
        rsx! {}
    }

    let dom = VirtualDom::new(app);
    dom.in_scope(ScopeId::ROOT, f)
}

#[test]
fn vec_store_parent_access_updates_parent_state() {
    run_with_runtime(|| {
        let root = Store::new(vec![10, 20, 30]);
        let second = root.clone().index(1);

        // The key reported by the lens should be the logical index.
        assert_eq!(second.key(), 1usize);
        // Path keys are implementation details but must exist for derived children.
        assert!(second.parent_path_key().is_some());

        // Access the parent handle and mutate it by removing the indexed item.
        let removed = second
            .with_parent(|index, mut parent| {
                assert_eq!(parent.len(), 3);
                assert_eq!(index, 1);
                parent.remove(index)
            })
            .expect("store should have a parent");
        assert_eq!(removed, 20);
        assert_eq!(root.len(), 2);

        // The parent_store helper mirrors with_parent but without a closure.
        let parent = second.parent_store().expect("parent should exist");
        assert_eq!(parent.len(), 2);
    });
}

#[test]
fn hashmap_store_parent_access_uses_logical_keys() {
    run_with_runtime(|| {
        let mut data = HashMap::new();
        data.insert("alpha".to_string(), 1);
        data.insert("beta".to_string(), 2);
        let root = Store::new(data);

        let beta = root.clone().get("beta".to_string()).unwrap();
        assert_eq!(beta.key(), "beta".to_string());
        assert!(beta.parent_path_key().is_some());

        // Remove the item through the parent store using the exposed key.
        beta.with_parent(|key, mut parent| {
            assert_eq!(parent.len(), 2);
            assert_eq!(parent.remove(&key), Some(2));
        })
        .expect("parent should exist");

        assert!(root.get("beta".to_string()).is_none());
        assert_eq!(root.len(), 1);
    });
}

#[test]
fn indexed_store_item_aliases_are_usable() {
    run_with_runtime(|| {
        let parent = Store::new(vec![42]);

        // Alias resolves to Store<i32, IndexWrite<usize, _>>.
        let item: IndexedStoreItem<i32, usize, _> = parent.clone().index(0);
        assert_eq!(item(), 42);

        // Hash map alias should also compile and behave identically.
        let mut map = HashMap::new();
        map.insert("answer".to_string(), 42);
        let map_store = Store::new(map);
        let hash_item: HashMapStoreItem<_, String, _> =
            map_store.clone().get("answer".to_string()).unwrap();
        assert_eq!(hash_item(), 42);

        // Ensure parent access works through aliases by reading through the parent store.
        let parent_value = hash_item
            .with_parent(|key, parent| parent.get(key).unwrap()())
            .expect("parent should exist");
        assert_eq!(parent_value, 42);
    });
}
