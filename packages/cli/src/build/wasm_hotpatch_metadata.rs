use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::Result;

const SAVED_WBG_PREFIX: &str = "__saved_wbg_";

#[derive(Debug, Default)]
pub(crate) struct WasmHotpatchMetadata {
    bindgen_symbol_set: HashSet<String>,
    cast_mappings: HashMap<String, Vec<String>>,
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

        match Self::from_sidecar_bytes(&sidecar_path, &sidecar_bytes) {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::warn!("{err}");
                Self::default()
            }
        }
    }

    pub(crate) fn load_required_for_base_wasm(base_wasm_path: &Path) -> Result<Self> {
        let sidecar_path = base_wasm_path.with_extension("hotpatch-metadata.json");
        let sidecar_bytes = std::fs::read(&sidecar_path).map_err(|err| {
            anyhow::anyhow!(
                "Failed to read required wasm hotpatch metadata sidecar at {}: {err}",
                sidecar_path.display()
            )
        })?;

        Self::from_sidecar_bytes(&sidecar_path, &sidecar_bytes)
    }

    fn from_sidecar_bytes(sidecar_path: &Path, sidecar_bytes: &[u8]) -> Result<Self> {
        let raw: RawWasmHotpatchMetadata =
            serde_json::from_slice(sidecar_bytes).map_err(|err| {
                anyhow::anyhow!(
                    "Failed to parse wasm hotpatch metadata sidecar at {}: {err}",
                    sidecar_path.display()
                )
            })?;
        let bindgen_symbol_set = raw.bindgen_symbol_set.into_iter().collect::<HashSet<_>>();
        let cast_mappings = raw
            .cast_mappings
            .into_iter()
            .map(|mapping| {
                (
                    mapping.generated_import_name,
                    mapping.original_function_names,
                )
            })
            .collect::<HashMap<_, _>>();
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

        Ok(Self {
            bindgen_symbol_set,
            cast_mappings,
            placeholder_import_names,
            externref_shim_map,
            import_renames,
        })
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
}
