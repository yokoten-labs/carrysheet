//! Merged cell ranges.
//!
//! Numbers records merges in one of three places, depending on the version
//! that saved the document. Like numbers-parser (MIT; see THIRD-PARTY.md),
//! this reader takes the first that has any: the table's merge owner
//! formula store, the merge owner's range dependencies in the calculation
//! engine, and the older merge region map.

use std::collections::HashMap;
use std::fmt;

use super::Warnings;
use super::formula::column_name;
use crate::container::Limits;
use crate::error::NumbersError;
use crate::iwa::{Store, types};
use crate::proto::{tsce, tsp, tst};

/// A rectangle of cells, counted from 0. Both ends are included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRange {
    /// The first row.
    pub first_row: u32,
    /// The first column.
    pub first_column: u32,
    /// The last row.
    pub last_row: u32,
    /// The last column.
    pub last_column: u32,
}

impl CellRange {
    /// True when the cell is inside the range.
    pub fn contains(&self, row: u32, column: u32) -> bool {
        (self.first_row..=self.last_row).contains(&row)
            && (self.first_column..=self.last_column).contains(&column)
    }
}

impl fmt::Display for CellRange {
    /// A1 notation, such as `B5:E5`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}:{}{}",
            column_name(self.first_column),
            u64::from(self.first_row) + 1,
            column_name(self.last_column),
            u64::from(self.last_row) + 1
        )
    }
}

/// Owner kinds in `TSCE.FormulaOwnerDependenciesArchive`.
const MERGE_OWNER: u32 = 5;
const HAUNTED_OWNER: u32 = 35;
/// `COLON_TRACT_NODE` in a formula.
const COLON_TRACT: i32 = 67;

pub(crate) fn uuid(u: &tsp::Uuid) -> u128 {
    u128::from(u.upper) << 64 | u128::from(u.lower)
}

pub(crate) fn cfuuid(u: &tsp::CfuuidArchive) -> u128 {
    u128::from(u.uuid_w3.unwrap_or(0)) << 96
        | u128::from(u.uuid_w2.unwrap_or(0)) << 64
        | u128::from(u.uuid_w1.unwrap_or(0)) << 32
        | u128::from(u.uuid_w0.unwrap_or(0))
}

/// What the calculation engine says about tables and merges, read once
/// per document.
#[derive(Default)]
pub(crate) struct Owners {
    /// Internal owner number to owner id.
    owner_ids: HashMap<u32, u128>,
    /// Formula owner id of a table's haunted owner to the table's base id.
    base_ids: HashMap<u128, u128>,
    /// Merged ranges by the owner id they belong to, grouped once so each
    /// table finds its own without looking at the others.
    merges: HashMap<u128, Vec<CellRange>>,
}

fn too_many_merges(what: &'static str, limit: usize) -> NumbersError {
    NumbersError::TooLarge {
        what,
        limit: limit as u64,
    }
}

impl Owners {
    /// Reads the calculation engine. Damage there is a warning (merged
    /// cells may be missing); only going over a limit is an error.
    pub(crate) fn load(
        store: &Store,
        limits: &Limits,
        warnings: &mut Warnings,
    ) -> Result<Self, NumbersError> {
        let mut owners = Self::default();
        let Some(engine_id) = store.ids_of_kind(types::CALCULATION_ENGINE.ids).next() else {
            return Ok(owners);
        };
        let engine: tsce::CalculationEngineArchive =
            match store.get(engine_id, &types::CALCULATION_ENGINE, String::new) {
                Ok(engine) => engine,
                Err(e @ NumbersError::TooLarge { .. }) => return Err(e),
                Err(e) => {
                    warnings.document(format!("merged cells may be missing: {}", e.what()));
                    return Ok(owners);
                }
            };
        if let Some(map) = &engine.dependency_tracker.owner_id_map {
            for entry in &map.map_entry {
                owners
                    .owner_ids
                    .insert(entry.internal_owner_id, cfuuid(&entry.owner_id));
            }
        }
        let mut merge_deps = Vec::new();
        for id in store.ids_of_kind(types::FORMULA_OWNER_DEPENDENCIES.ids) {
            let deps: tsce::FormulaOwnerDependenciesArchive =
                match store.get(id, &types::FORMULA_OWNER_DEPENDENCIES, String::new) {
                    Ok(deps) => deps,
                    Err(e @ NumbersError::TooLarge { .. }) => return Err(e),
                    Err(e) => {
                        warnings.document(format!("merged cells may be missing: {}", e.what()));
                        continue;
                    }
                };
            match deps.owner_kind.unwrap_or(0) {
                HAUNTED_OWNER => {
                    if let Some(base) = &deps.base_owner_uid {
                        owners
                            .base_ids
                            .insert(uuid(&deps.formula_owner_uid), uuid(base));
                    }
                }
                MERGE_OWNER => merge_deps.push(deps),
                _ => {}
            }
        }
        let mut count = 0usize;
        for deps in merge_deps {
            let Some(ranges) = &deps.range_dependencies else {
                continue;
            };
            for record in &ranges.back_dependency {
                let Some(reference) = &record.internal_range_reference else {
                    continue;
                };
                let Some(&owner) = owners.owner_ids.get(&reference.owner_id) else {
                    continue;
                };
                count += 1;
                if count > limits.max_document_merges {
                    return Err(too_many_merges(
                        "merged ranges in the document",
                        limits.max_document_merges,
                    ));
                }
                let r = &reference.range;
                owners.merges.entry(owner).or_default().push(CellRange {
                    first_row: r.top_left_row,
                    first_column: r.top_left_column,
                    last_row: r.bottom_right_row,
                    last_column: r.bottom_right_column,
                });
            }
        }
        Ok(owners)
    }

    /// The id other tables and the merge records use for a table.
    pub(crate) fn table_uid(&self, model: &tst::TableModelArchive) -> Option<u128> {
        let haunted = model.haunted_owner.as_ref()?;
        self.base_ids.get(&uuid(&haunted.owner_uid)).copied()
    }
}

/// The merged ranges of one table, as stored (not yet checked), at most
/// `limit` of them.
pub(crate) fn table_merges(
    store: &Store,
    owners: &Owners,
    model: &tst::TableModelArchive,
    needed_for: &str,
    limit: usize,
) -> Result<Vec<CellRange>, NumbersError> {
    let full = |found: &Vec<CellRange>| {
        if found.len() >= limit {
            Err(too_many_merges("merged ranges in one table", limit))
        } else {
            Ok(())
        }
    };
    // 1. The merge owner's formula store.
    let mut found = Vec::new();
    if let Some(stored) = model
        .merge_owner
        .as_ref()
        .and_then(|m| m.formula_store.as_ref())
    {
        for pair in &stored.formulas {
            let Some(node) = pair.formula.ast_node_array.ast_node.first() else {
                continue;
            };
            if node.ast_node_type != COLON_TRACT {
                continue;
            }
            let Some(tract) = &node.ast_colon_tract else {
                continue;
            };
            let (Some(rows), Some(cols)) =
                (tract.absolute_row.first(), tract.absolute_column.first())
            else {
                continue;
            };
            full(&found)?;
            found.push(CellRange {
                first_row: rows.range_begin,
                first_column: cols.range_begin,
                last_row: rows.range_end.unwrap_or(rows.range_begin),
                last_column: cols.range_end.unwrap_or(cols.range_begin),
            });
        }
    }
    if !found.is_empty() {
        return Ok(found);
    }
    // 2. The calculation engine's merge owners.
    if let Some(ranges) = owners
        .table_uid(model)
        .and_then(|uid| owners.merges.get(&uid))
    {
        if ranges.len() > limit {
            return Err(too_many_merges("merged ranges in one table", limit));
        }
        found.extend_from_slice(ranges);
    }
    if !found.is_empty() {
        return Ok(found);
    }
    // 3. The merge region map.
    let Some(map) = model.base_data_store.merge_region_map.as_ref() else {
        return Ok(found);
    };
    if map.identifier == 0 {
        return Ok(found);
    }
    let map: tst::MergeRegionMapArchive =
        store.get(map.identifier, &types::MERGE_REGION_MAP, || {
            needed_for.to_owned()
        })?;
    for range in &map.cell_range {
        let origin = range.origin.packed_data;
        let size = range.size.packed_data;
        let (columns, rows) = (size >> 16, size & 0xffff);
        if columns == 0 || rows == 0 {
            continue;
        }
        let (first_column, first_row) = (origin >> 16, origin & 0xffff);
        full(&found)?;
        found.push(CellRange {
            first_row,
            first_column,
            last_row: first_row + rows - 1,
            last_column: first_column + columns - 1,
        });
    }
    Ok(found)
}
