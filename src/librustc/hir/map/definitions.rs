// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.


//! For each definition, we track the following data.  A definition
//! here is defined somewhat circularly as "something with a def-id",
//! but it generally corresponds to things like structs, enums, etc.
//! There are also some rather random cases (like const initializer
//! expressions) that are mostly just leftovers.



use hir::def_id::{CrateNum, DefId, DefIndex, LOCAL_CRATE};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::stable_hasher::StableHasher;
use serialize::{Encodable, Decodable, Encoder, Decoder};
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use syntax::ast;
use syntax::symbol::{Symbol, InternedString};
use ty::TyCtxt;
use util::nodemap::NodeMap;

#[derive(Clone)]
pub struct DefPathTable {
    index_to_key: Vec<DefKey>,
    key_to_index: FxHashMap<DefKey, DefIndex>,
}

impl DefPathTable {
    fn insert(&mut self, key: DefKey) -> DefIndex {
        let index = DefIndex::new(self.index_to_key.len());
        debug!("DefPathTable::insert() - {:?} <-> {:?}", key, index);
        self.index_to_key.push(key.clone());
        self.key_to_index.insert(key, index);
        index
    }

    #[inline(always)]
    pub fn def_key(&self, index: DefIndex) -> DefKey {
        self.index_to_key[index.as_usize()].clone()
    }

    #[inline(always)]
    pub fn def_index_for_def_key(&self, key: &DefKey) -> Option<DefIndex> {
        self.key_to_index.get(key).cloned()
    }

    #[inline(always)]
    pub fn contains_key(&self, key: &DefKey) -> bool {
        self.key_to_index.contains_key(key)
    }

    /// Returns the path from the crate root to `index`. The root
    /// nodes are not included in the path (i.e., this will be an
    /// empty vector for the crate root). For an inlined item, this
    /// will be the path of the item in the external crate (but the
    /// path will begin with the path to the external crate).
    pub fn def_path(&self, index: DefIndex) -> DefPath {
        DefPath::make(LOCAL_CRATE, index, |p| self.def_key(p))
    }
}


impl Encodable for DefPathTable {
    fn encode<S: Encoder>(&self, s: &mut S) -> Result<(), S::Error> {
        self.index_to_key.encode(s)
    }
}

impl Decodable for DefPathTable {
    fn decode<D: Decoder>(d: &mut D) -> Result<DefPathTable, D::Error> {
        let index_to_key: Vec<DefKey> = Decodable::decode(d)?;
        let key_to_index = index_to_key.iter()
                                       .enumerate()
                                       .map(|(index, key)| (key.clone(), DefIndex::new(index)))
                                       .collect();
        Ok(DefPathTable {
            index_to_key: index_to_key,
            key_to_index: key_to_index,
        })
    }
}


/// The definition table containing node definitions
#[derive(Clone)]
pub struct Definitions {
    table: DefPathTable,
    node_to_def_index: NodeMap<DefIndex>,
    def_index_to_node: Vec<ast::NodeId>,
}

/// A unique identifier that we can use to lookup a definition
/// precisely. It combines the index of the definition's parent (if
/// any) with a `DisambiguatedDefPathData`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RustcEncodable, RustcDecodable)]
pub struct DefKey {
    /// Parent path.
    pub parent: Option<DefIndex>,

    /// Identifier of this node.
    pub disambiguated_data: DisambiguatedDefPathData,
}

/// Pair of `DefPathData` and an integer disambiguator. The integer is
/// normally 0, but in the event that there are multiple defs with the
/// same `parent` and `data`, we use this field to disambiguate
/// between them. This introduces some artificial ordering dependency
/// but means that if you have (e.g.) two impls for the same type in
/// the same module, they do get distinct def-ids.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RustcEncodable, RustcDecodable)]
pub struct DisambiguatedDefPathData {
    pub data: DefPathData,
    pub disambiguator: u32
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, RustcEncodable, RustcDecodable)]
pub struct DefPath {
    /// the path leading from the crate root to the item
    pub data: Vec<DisambiguatedDefPathData>,

    /// what krate root is this path relative to?
    pub krate: CrateNum,
}

impl DefPath {
    pub fn is_local(&self) -> bool {
        self.krate == LOCAL_CRATE
    }

    pub fn make<FN>(start_krate: CrateNum,
                    start_index: DefIndex,
                    mut get_key: FN) -> DefPath
        where FN: FnMut(DefIndex) -> DefKey
    {
        let mut krate = start_krate;
        let mut data = vec![];
        let mut index = Some(start_index);
        loop {
            debug!("DefPath::make: krate={:?} index={:?}", krate, index);
            let p = index.unwrap();
            let key = get_key(p);
            debug!("DefPath::make: key={:?}", key);
            match key.disambiguated_data.data {
                DefPathData::CrateRoot => {
                    assert!(key.parent.is_none());
                    break;
                }
                DefPathData::InlinedRoot(ref p) => {
                    assert!(key.parent.is_none());
                    assert!(!p.def_id.is_local());
                    data.extend(p.data.iter().cloned().rev());
                    krate = p.def_id.krate;
                    break;
                }
                _ => {
                    data.push(key.disambiguated_data);
                    index = key.parent;
                }
            }
        }
        data.reverse();
        DefPath { data: data, krate: krate }
    }

    pub fn to_string(&self, tcx: TyCtxt) -> String {
        let mut s = String::with_capacity(self.data.len() * 16);

        s.push_str(&tcx.original_crate_name(self.krate).as_str());
        s.push_str("/");
        s.push_str(&tcx.crate_disambiguator(self.krate).as_str());

        for component in &self.data {
            write!(s,
                   "::{}[{}]",
                   component.data.as_interned_str(),
                   component.disambiguator)
                .unwrap();
        }

        s
    }

    pub fn deterministic_hash(&self, tcx: TyCtxt) -> u64 {
        debug!("deterministic_hash({:?})", self);
        let mut state = StableHasher::new();
        self.deterministic_hash_to(tcx, &mut state);
        state.finish()
    }

    pub fn deterministic_hash_to<H: Hasher>(&self, tcx: TyCtxt, state: &mut H) {
        tcx.original_crate_name(self.krate).as_str().hash(state);
        tcx.crate_disambiguator(self.krate).as_str().hash(state);
        self.data.hash(state);
    }
}

/// Root of an inlined item. We track the `DefPath` of the item within
/// the original crate but also its def-id. This is kind of an
/// augmented version of a `DefPath` that includes a `DefId`. This is
/// all sort of ugly but the hope is that inlined items will be going
/// away soon anyway.
///
/// Some of the constraints that led to the current approach:
///
/// - I don't want to have a `DefId` in the main `DefPath` because
///   that gets serialized for incr. comp., and when reloaded the
///   `DefId` is no longer valid. I'd rather maintain the invariant
///   that every `DefId` is valid, and a potentially outdated `DefId` is
///   represented as a `DefPath`.
///   - (We don't serialize def-paths from inlined items, so it's ok to have one here.)
/// - We need to be able to extract the def-id from inline items to
///   make the symbol name. In theory we could retrace it from the
///   data, but the metadata doesn't have the required indices, and I
///   don't want to write the code to create one just for this.
/// - It may be that we don't actually need `data` at all. We'll have
///   to see about that.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RustcEncodable, RustcDecodable)]
pub struct InlinedRootPath {
    pub data: Vec<DisambiguatedDefPathData>,
    pub def_id: DefId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, RustcEncodable, RustcDecodable)]
pub enum DefPathData {
    // Root: these should only be used for the root nodes, because
    // they are treated specially by the `def_path` function.
    /// The crate root (marker)
    CrateRoot,
    /// An inlined root
    InlinedRoot(Box<InlinedRootPath>),

    // Catch-all for random DefId things like DUMMY_NODE_ID
    Misc,

    // Different kinds of items and item-like things:
    /// An impl
    Impl,
    /// Something in the type NS
    TypeNs(InternedString),
    /// Something in the value NS
    ValueNs(InternedString),
    /// A module declaration
    Module(InternedString),
    /// A macro rule
    MacroDef(InternedString),
    /// A closure expression
    ClosureExpr,

    // Subportions of items
    /// A type parameter (generic parameter)
    TypeParam(InternedString),
    /// A lifetime definition
    LifetimeDef(InternedString),
    /// A variant of a enum
    EnumVariant(InternedString),
    /// A struct field
    Field(InternedString),
    /// Implicit ctor for a tuple-like struct
    StructCtor,
    /// Initializer for a const
    Initializer,
    /// Pattern binding
    Binding(InternedString),
    /// An `impl Trait` type node.
    ImplTrait
}

impl Definitions {
    /// Create new empty definition map.
    pub fn new() -> Definitions {
        Definitions {
            table: DefPathTable {
                index_to_key: vec![],
                key_to_index: FxHashMap(),
            },
            node_to_def_index: NodeMap(),
            def_index_to_node: vec![],
        }
    }

    /// Get the number of definitions.
    pub fn len(&self) -> usize {
        self.def_index_to_node.len()
    }

    pub fn def_key(&self, index: DefIndex) -> DefKey {
        self.table.def_key(index)
    }

    pub fn def_index_for_def_key(&self, key: DefKey) -> Option<DefIndex> {
        self.table.def_index_for_def_key(&key)
    }

    /// Returns the path from the crate root to `index`. The root
    /// nodes are not included in the path (i.e., this will be an
    /// empty vector for the crate root). For an inlined item, this
    /// will be the path of the item in the external crate (but the
    /// path will begin with the path to the external crate).
    pub fn def_path(&self, index: DefIndex) -> DefPath {
        DefPath::make(LOCAL_CRATE, index, |p| self.def_key(p))
    }

    pub fn opt_def_index(&self, node: ast::NodeId) -> Option<DefIndex> {
        self.node_to_def_index.get(&node).cloned()
    }

    pub fn opt_local_def_id(&self, node: ast::NodeId) -> Option<DefId> {
        self.opt_def_index(node).map(DefId::local)
    }

    pub fn local_def_id(&self, node: ast::NodeId) -> DefId {
        self.opt_local_def_id(node).unwrap()
    }

    pub fn as_local_node_id(&self, def_id: DefId) -> Option<ast::NodeId> {
        if def_id.krate == LOCAL_CRATE {
            assert!(def_id.index.as_usize() < self.def_index_to_node.len());
            Some(self.def_index_to_node[def_id.index.as_usize()])
        } else {
            None
        }
    }

    /// Add a definition with a parent definition.
    pub fn create_def_with_parent(&mut self,
                                  parent: Option<DefIndex>,
                                  node_id: ast::NodeId,
                                  data: DefPathData)
                                  -> DefIndex {
        debug!("create_def_with_parent(parent={:?}, node_id={:?}, data={:?})",
               parent, node_id, data);

        assert!(!self.node_to_def_index.contains_key(&node_id),
                "adding a def'n for node-id {:?} and data {:?} but a previous def'n exists: {:?}",
                node_id,
                data,
                self.table.def_key(self.node_to_def_index[&node_id]));

        assert!(parent.is_some() ^ match data {
            DefPathData::CrateRoot | DefPathData::InlinedRoot(_) => true,
            _ => false,
        });

        // Find a unique DefKey. This basically means incrementing the disambiguator
        // until we get no match.
        let mut key = DefKey {
            parent: parent,
            disambiguated_data: DisambiguatedDefPathData {
                data: data,
                disambiguator: 0
            }
        };

        while self.table.contains_key(&key) {
            key.disambiguated_data.disambiguator += 1;
        }

        debug!("create_def_with_parent: after disambiguation, key = {:?}", key);

        // Create the definition.
        let index = self.table.insert(key);
        debug!("create_def_with_parent: def_index_to_node[{:?} <-> {:?}", index, node_id);
        self.node_to_def_index.insert(node_id, index);
        assert_eq!(index.as_usize(), self.def_index_to_node.len());
        self.def_index_to_node.push(node_id);

        index
    }
}

impl DefPathData {
    pub fn get_opt_name(&self) -> Option<ast::Name> {
        use self::DefPathData::*;
        match *self {
            TypeNs(ref name) |
            ValueNs(ref name) |
            Module(ref name) |
            MacroDef(ref name) |
            TypeParam(ref name) |
            LifetimeDef(ref name) |
            EnumVariant(ref name) |
            Binding(ref name) |
            Field(ref name) => Some(Symbol::intern(name)),

            Impl |
            CrateRoot |
            InlinedRoot(_) |
            Misc |
            ClosureExpr |
            StructCtor |
            Initializer |
            ImplTrait => None
        }
    }

    pub fn as_interned_str(&self) -> InternedString {
        use self::DefPathData::*;
        let s = match *self {
            TypeNs(ref name) |
            ValueNs(ref name) |
            Module(ref name) |
            MacroDef(ref name) |
            TypeParam(ref name) |
            LifetimeDef(ref name) |
            EnumVariant(ref name) |
            Binding(ref name) |
            Field(ref name) => {
                return name.clone();
            }

            // note that this does not show up in user printouts
            CrateRoot => "{{root}}",

            // note that this does not show up in user printouts
            InlinedRoot(_) => "{{inlined-root}}",

            Impl => "{{impl}}",
            Misc => "{{?}}",
            ClosureExpr => "{{closure}}",
            StructCtor => "{{constructor}}",
            Initializer => "{{initializer}}",
            ImplTrait => "{{impl-Trait}}",
        };

        Symbol::intern(s).as_str()
    }

    pub fn to_string(&self) -> String {
        self.as_interned_str().to_string()
    }
}
