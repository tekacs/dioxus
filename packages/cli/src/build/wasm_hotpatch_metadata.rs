use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const SAVED_WBG_PREFIX: &str = "__saved_wbg_";

#[derive(Debug, Default)]
pub(crate) struct WasmHotpatchMetadata {
    bindgen_symbol_set: HashSet<String>,
    cast_mappings: HashMap<String, Vec<String>>,
    cast_generated_imports_by_old_name: HashMap<String, String>,
    placeholder_import_names: HashSet<String>,
    pub(crate) externref_shim_map: HashMap<String, String>,
    /// Maps original import names to their final (renamed) names in the post-bindgen module.
    /// e.g. "__wbindgen_object_clone_ref" → "__wbindgen_object_clone_ref_unused"
    import_renames: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawWasmHotpatchMetadata {
    #[serde(default)]
    bindgen_symbol_set: Vec<String>,
    #[serde(default)]
    cast_mappings: Vec<RawHotpatchCastMapping>,
    #[serde(default)]
    placeholder_import_mappings: Vec<RawHotpatchPlaceholderMapping>,
    #[serde(default)]
    externref_import_shims: Vec<RawExternrefShimMapping>,
    #[serde(default)]
    import_renames: Vec<RawImportRename>,
}

#[derive(Debug, Deserialize)]
struct RawExternrefShimMapping {
    #[serde(default)]
    original_func_name: Option<String>,
    shim_func_name: String,
}

#[derive(Debug, Deserialize)]
struct RawHotpatchCastMapping {
    generated_import_name: String,
    #[serde(default)]
    original_function_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawHotpatchPlaceholderMapping {
    import_name: String,
}

#[derive(Debug, Deserialize)]
struct RawImportRename {
    original_name: String,
    final_name: String,
}

impl WasmHotpatchMetadata {
    pub(crate) fn load_for_base_wasm(base_wasm_path: &Path) -> Self {
        let sidecar_path = base_wasm_path.with_extension("hotpatch-metadata.json");
        let sidecar_bytes = match std::fs::read(&sidecar_path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    "No wasm hotpatch metadata sidecar found at {}",
                    sidecar_path.display()
                );
                return Self::default();
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to read wasm hotpatch metadata sidecar at {}: {err}",
                    sidecar_path.display()
                );
                return Self::default();
            }
        };

        let raw: RawWasmHotpatchMetadata = match serde_json::from_slice(&sidecar_bytes) {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::warn!(
                    "Failed to parse wasm hotpatch metadata sidecar at {}: {err}",
                    sidecar_path.display()
                );
                return Self::default();
            }
        };

        let bindgen_symbol_set = raw.bindgen_symbol_set.into_iter().collect::<HashSet<_>>();
        let mut cast_mappings = HashMap::new();
        let mut cast_generated_imports_by_old_name = HashMap::new();
        for mapping in raw.cast_mappings {
            let generated_import_name = mapping.generated_import_name;
            for old_name in &mapping.original_function_names {
                cast_generated_imports_by_old_name
                    .insert(old_name.clone(), generated_import_name.clone());
            }
            cast_mappings.insert(generated_import_name, mapping.original_function_names);
        }
        let placeholder_import_names = raw
            .placeholder_import_mappings
            .into_iter()
            .map(|mapping| mapping.import_name)
            .collect::<HashSet<_>>();

        let externref_shim_map = raw
            .externref_import_shims
            .into_iter()
            .filter_map(|m| Some((m.original_func_name?, m.shim_func_name)))
            .collect::<HashMap<_, _>>();

        let import_renames = raw
            .import_renames
            .into_iter()
            .map(|r| (r.original_name, r.final_name))
            .collect::<HashMap<_, _>>();

        Self {
            bindgen_symbol_set,
            cast_mappings,
            cast_generated_imports_by_old_name,
            placeholder_import_names,
            externref_shim_map,
            import_renames,
        }
    }

    pub(crate) fn is_bindgen_symbol(&self, symbol_name: &str) -> bool {
        self.bindgen_symbol_set.contains(symbol_name)
            || self
                .bindgen_symbol_set
                .contains(symbol_name.trim_start_matches(SAVED_WBG_PREFIX))
    }

    pub(crate) fn has_placeholder_import(&self, symbol_name: &str) -> bool {
        self.placeholder_import_names.contains(symbol_name)
            || self
                .placeholder_import_names
                .contains(symbol_name.trim_start_matches(SAVED_WBG_PREFIX))
    }

    /// Given an original import name, return the final name after wasm-bindgen renaming.
    /// Returns the original name unchanged if no rename was recorded.
    pub(crate) fn resolve_import_rename<'a>(&'a self, original_name: &'a str) -> &'a str {
        self.import_renames
            .get(original_name)
            .map(|s| s.as_str())
            .unwrap_or(original_name)
    }

    pub(crate) fn cast_target_ifunc_index(
        &self,
        import_name: &str,
        name_to_ifunc_old: &HashMap<String, i32>,
    ) -> Option<i32> {
        let import_name = import_name.trim_start_matches(SAVED_WBG_PREFIX);
        let old_names = self.cast_mappings.get(import_name)?;
        old_names
            .iter()
            .find_map(|old_name| name_to_ifunc_old.get(old_name).copied())
    }

    pub(crate) fn cast_generated_import_name_for_function<'a>(
        &'a self,
        function_name: &str,
    ) -> Option<&'a str> {
        self.cast_generated_imports_by_old_name
            .get(function_name)
            .map(|name| name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_generated_cast_imports_from_original_function_names() {
        let old_name = "_ZN12wasm_bindgen4__rt8wbg_cast17breaks_if_inlined17h89ebd8cfd0f04703E";
        let generated_name = "__wbindgen_cast_000000000000000e";

        let metadata = WasmHotpatchMetadata {
            bindgen_symbol_set: HashSet::new(),
            cast_mappings: HashMap::from([(
                generated_name.to_string(),
                vec![old_name.to_string()],
            )]),
            cast_generated_imports_by_old_name: HashMap::from([(
                old_name.to_string(),
                generated_name.to_string(),
            )]),
            placeholder_import_names: HashSet::new(),
            externref_shim_map: HashMap::new(),
            import_renames: HashMap::new(),
        };

        assert_eq!(
            metadata.cast_generated_import_name_for_function(old_name),
            Some(generated_name)
        );
    }
}
