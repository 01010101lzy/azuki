//! TAC (Three-Address Code) is the intermediate representation for Azuki programs.
//! This code type is similar to [Cranelift IR][cranelift] and [LLVM IR][llvm].
//!
//! This crate contains definition of Azuki TAC, code to serialize and deserialize
//! them, and to construct them from regular control flows in a higher-level
//! programming language.
//!
//! See the details in separate modules.
//!
//! [cranelift]: https://github.com/bytecodealliance/wasmtime
//! [llvm]: https://llvm.org

pub mod builder;
pub mod containers;
pub mod err;
pub mod formatter;
mod linkedlist;
pub mod optimizer;
pub mod parser;
pub mod ty;
pub mod util;

use std::collections::{BTreeMap, HashMap};

use enum_as_inner::EnumAsInner;
use err::{Error, TacResult};

use linkedlist::{ImplicitLinkedList, ImplicitLinkedListItem};
use smol_str::SmolStr;
use thunderdome::{Arena, Index};

pub use ty::{NumericTy, Ty, TyKind};
use util::VarIter;

pub use containers::{BBId, InstId};

#[derive(Debug, Clone)]
pub struct Program {
    pub functions: HashMap<SmolStr, TacFunc>,
}

/// A function made of TAC instructions.
///
/// The instructions are represented as an indirect doubly linked list inside the
/// `arena` using item indices. Every basic block holds the start and end index
/// of its instructions.
///
/// **ALL methods of this type will panic if you feed invalid indices into them.**
/// If that happens, there's definitely bug inside your code.
#[derive(Debug, Clone, Default)]
pub struct TacFunc {
    /// Function name
    pub name: SmolStr,
    /// Function type
    pub ty: Ty,

    // The followings are allocating spaces for data types
    /// An arena to allocate instructions
    instructions_arena: Arena<Tac>,
    /// An arena to allocate basic block info
    basic_block_arena: Arena<BasicBlock>,

    pub first_block: Option<BBId>,
}

impl TacFunc {
    pub fn new(name: SmolStr, ty: Ty) -> TacFunc {
        // let mut basic_blocks = DiGraph::new();
        // let starting_block = basic_blocks.add_node(BasicBlock {
        //     head: None,
        //     tail: None,
        //     jumps: Default::default(),
        // });

        TacFunc {
            name,
            ty,
            instructions_arena: Arena::new(),
            basic_block_arena: Arena::new(),
            first_block: None,
        }
    }

    pub fn new_untyped(name: SmolStr) -> TacFunc {
        Self::new(name, Ty::unit())
    }

    pub fn starting_block(&self) -> Option<BBId> {
        self.first_block
    }
}

/// Methods for manipulating instructions inside a function.
///
/// Note: All functions are infallible, and would panic when you use it with an
/// ID that does not exist with it.
impl TacFunc {
    /// Insert a new TAC into arena with no next instruction, without a proper
    /// basic block ID.
    pub fn inst_new(&mut self, inst: Inst) -> InstId {
        self.instructions_arena
            .insert(Tac::independent(inst, BBId::default()))
    }

    pub fn inst_exists(&self, inst: InstId) -> bool {
        self.instructions_arena.get(inst).is_some()
    }

    pub fn inst_next(&self, inst: InstId) -> Option<InstId> {
        self.tac_get(inst).next
    }

    pub fn inst_prev(&self, inst: InstId) -> Option<InstId> {
        self.tac_get(inst).prev
    }

    /// Gets the instruction with extra information around it as a `Tac`
    #[inline]
    pub fn tac_get(&self, idx: InstId) -> &Tac {
        self.instructions_arena.get_item(idx)
    }

    /// Gets a mutable reference of the instruction with extra information around it
    /// as a `Tac`
    #[inline]
    pub fn tac_get_mut(&mut self, idx: InstId) -> &mut Tac {
        self.instructions_arena.get_item_mut(idx)
    }

    /// Get the instruction body
    #[inline]
    pub fn inst_get(&self, idx: InstId) -> &Inst {
        &self.tac_get(idx).inst
    }

    /// Get a mutable reference of the instruction body
    #[inline]
    pub fn inst_get_mut(&mut self, idx: InstId) -> &mut Inst {
        &mut self.tac_get_mut(idx).inst
    }

    /// Position this instruction after the given instruction.
    pub fn inst_set_after(&mut self, after: InstId, inst: InstId) {
        self.instructions_arena.attach_after(after, inst);
        let bb = self.tac_get(after).bb;
        self.tac_get_mut(inst).bb = bb;

        let bb = self.bb_get_mut(bb);
        if bb.tail == Some(after) {
            bb.tail = Some(inst);
        }
    }

    /// Position this instruction before the given instruction.
    pub fn inst_set_before(&mut self, before: InstId, inst: InstId) {
        self.instructions_arena.attach_before(before, inst);
        let bb = self.tac_get(before).bb;
        self.tac_get_mut(inst).bb = bb;

        let bb = self.bb_get_mut(bb);
        if bb.head == Some(before) {
            bb.head = Some(inst);
        }
    }

    /// Append the given instruction as the last instruction in basic block
    pub fn inst_append_in_bb(&mut self, inst: InstId, bb: BBId) {
        debug_assert!(self.tac_get(inst).is_freestanding());

        self.tac_get_mut(inst).bb = bb;
        let bb = self.bb_get_mut(bb);
        let old_tail = bb.tail.replace(inst);
        if bb.head.is_none() {
            bb.head = Some(inst);
        }
        if let Some(old_tail) = old_tail {
            self.inst_set_after(old_tail, inst);
        }
    }

    /// Prepend the given instruction as the first instruction in basic block
    pub fn inst_prepend_in_bb(&mut self, inst: InstId, bb: BBId) {
        debug_assert!(self.tac_get(inst).is_freestanding());

        self.tac_get_mut(inst).bb = bb;
        let bb = self.bb_get_mut(bb);
        let old_head = bb.head.replace(inst);
        if bb.tail.is_none() {
            bb.tail = Some(inst);
        }
        if let Some(old_head) = old_head {
            self.inst_set_before(old_head, inst);
        }
    }

    /// Detaches this instruction from the instruction chain.
    pub fn inst_detach(&mut self, idx: InstId) {
        self.instructions_arena.detach(idx);
        self.tac_get_mut(idx).bb = BBId::default();
    }

    /// Remove the given instruction
    pub fn inst_remove(&mut self, idx: InstId) -> Inst {
        debug_assert_eq!(
            self.inst_next(idx),
            None,
            "The instruction should be detached from the chain"
        );
        debug_assert_eq!(
            self.inst_prev(idx),
            None,
            "The instruction should be detached from the chain"
        );

        self.instructions_arena.remove(idx).unwrap().inst
    }

    /// Connect TAC instruction `head` to the place after `tail`.
    ///
    /// Errors if `tail` does not exist or `tail.next` is not `None`. _This function
    /// does not check for availability of `head`._
    ///
    /// # Panics
    ///
    /// This function requires `head` and `tail` to be different. Panics if
    /// `head == tail`.
    pub fn inst_connect(&mut self, tail: InstId, head: InstId) {
        assert_ne!(tail, head, "Can't connect one same instruction!");
        self.instructions_arena.connect(tail, head);
    }

    /// Splits off instruction chain after position `pos`. Returns the index of
    /// head instruction of the latter chain.
    ///
    /// Errors if `pos` does not exist or there is no code after `tail`.
    fn inst_split_off_after(&mut self, pos: InstId) -> Option<InstId> {
        let tail = self.tac_get_mut(pos);
        tail.next.take()
    }

    /// Returns an iterator of all instructions inside this function, unordered.
    pub fn all_inst_unordered(&self) -> impl Iterator<Item = (InstId, BBId, &Inst)> {
        self.instructions_arena
            .iter()
            .map(|(idx, inst)| (idx, inst.bb, &inst.inst))
    }
}

/// Methods for playing with basic blocks
impl TacFunc {
    /// Insert a new basic block into this function
    pub fn bb_new(&mut self) -> BBId {
        self.basic_block_arena.insert(BasicBlock::default()).into()
    }

    pub fn bb_exists(&self, idx: BBId) -> bool {
        self.basic_block_arena.get(idx.into()).is_some()
    }

    #[inline]
    pub fn bb_get(&self, idx: BBId) -> &BasicBlock {
        &self.basic_block_arena[idx]
    }

    #[inline]
    pub fn bb_get_mut(&mut self, idx: BBId) -> &mut BasicBlock {
        &mut self.basic_block_arena[idx]
    }

    #[inline]
    pub fn bb_get2_mut(&mut self, i1: BBId, i2: BBId) -> (&mut BasicBlock, &mut BasicBlock) {
        let (v1, v2) = self.basic_block_arena.get2_mut(i1.into(), i2.into());
        (
            v1.unwrap_or_else(|| panic!("No index at {:?}", i1)),
            v2.unwrap_or_else(|| panic!("No index at {:?}", i2)),
        )
    }

    /// Set the given block as the first block in function.
    /// Returns the previous first block.
    pub fn bb_set_first(&mut self, bb: BBId) -> Option<BBId> {
        debug_assert!(self.bb_exists(bb));
        self.first_block.replace(bb)
    }

    pub fn bb_set_before(&mut self, before: BBId, bb: BBId) {
        self.basic_block_arena.attach_before(before, bb);
    }

    pub fn bb_set_after(&mut self, after: BBId, bb: BBId) {
        self.basic_block_arena.attach_after(after, bb);
    }

    pub fn bb_detach(&mut self, bb: BBId) {
        self.basic_block_arena.detach(bb);
    }

    #[inline]
    pub fn all_bb_unordered(&self) -> impl Iterator<Item = (BBId, &BasicBlock)> {
        self.basic_block_arena
            .iter()
            .map(|(idx, bb)| (idx.into(), bb))
    }

    /// Split all instruction after `inst` into a new basic block. Returns the ID
    /// of that basic block.
    ///
    /// - Creates a new basic block `new_bb`
    /// - Move every instruction after `inst` and inside its basic block to `new_bb`
    /// - Move every jump instruction to `new_bb` if `transfer_branches` is `true`
    pub fn bb_split_after(&mut self, inst: InstId, transfer_branches: bool) -> BBId {
        let after_head = self.inst_split_off_after(inst);
        let first_bb_id = self.tac_get(inst).bb;
        let first_bb = self.bb_get_mut(first_bb_id);
        let orig_tail = first_bb.tail.take();

        let jumps = transfer_branches
            .then(|| std::mem::replace(&mut first_bb.jumps, vec![]))
            .unwrap_or_else(Vec::new);
        let new_bb_id = self.bb_new();

        let new_bb = self.bb_get_mut(new_bb_id);
        new_bb.tail = orig_tail;
        new_bb.head = after_head;
        new_bb.jumps = jumps;

        {
            // fix bb pointers
            let mut it = new_bb.head;
            while let Some(inst) = it {
                let tac = self.tac_get_mut(inst);
                tac.bb = new_bb_id;
                it = tac.next();
            }
        }

        new_bb_id
    }

    /// Concatenate basic block `back` into `front` and detaches `back` from
    /// basic blocks. Returns branch instruction inside `front`.
    ///
    /// - Move every instruction inside `back` into `front`
    /// - Remove all jump instruction inside `front`
    /// - Move all instrcution inside `back` into `front`
    /// - Detach `back`
    pub fn bb_connect(&mut self, front: BBId, back: BBId) -> Vec<Branch> {
        debug_assert_ne!(front, back, "Cannot connect a basic block to itself");

        let (front_bb, back_bb) = self.bb_get2_mut(front, back);

        let back_jump = std::mem::take(&mut back_bb.jumps);
        let branches = std::mem::replace(&mut front_bb.jumps, back_jump);

        let front_tail = front_bb.tail;
        let back_head = back_bb.head;

        if let Some(head) = back_head {
            if let Some(tail) = front_tail {
                front_bb.tail = back_bb.tail.take();
                back_bb.head = None;
                self.inst_connect(tail, head);
            } else {
                // `front` is empty, simply move `head` and `tail` around
                front_bb.head = back_bb.head.take();
                front_bb.tail = back_bb.tail.take();
            }
            {
                // fix bb pointers
                let mut it = back_head;
                while let Some(inst) = it {
                    let tac = self.tac_get_mut(inst);
                    tac.bb = front;
                    it = tac.next();
                }
            }
        }

        branches
    }

    pub fn bb_iter(&self) -> impl Iterator<Item = (BBId, &BasicBlock)> {
        self.basic_block_arena.items_iter(self.first_block, None)
    }
}

/// A single basic block, represented as an indirect doubly linked list of instructions.
#[derive(Debug, Clone, Default)]
pub struct BasicBlock {
    /// Linked list head
    pub head: Option<InstId>,
    /// Linked list tail
    pub tail: Option<InstId>,

    /// Linked list head
    pub prev: Option<BBId>,
    /// Linked list tail
    pub next: Option<BBId>,

    /// The branch instruction at the end of this basic block
    pub jumps: Vec<Branch>,
}

/// Represents a single TAC instruction inside an indirect doubly linked list of instructions.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Tac {
    /// The actual instruction.
    pub inst: Inst,

    /// The basic block this instruction is in.
    pub bb: BBId,

    /// The previous instruction in this list.
    pub prev: Option<InstId>,
    /// The next instruction in this list.
    pub next: Option<InstId>,
}

impl Tac {
    pub fn new(inst: Inst, prev: Option<InstId>, next: Option<InstId>, bb: BBId) -> Self {
        Self {
            inst,
            prev,
            next,
            bb,
        }
    }

    pub fn independent(inst: Inst, bb: BBId) -> Tac {
        Tac {
            inst,
            bb,
            prev: None,
            next: None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BinaryInst {
    pub op: BinaryOp,
    pub lhs: Value,
    pub rhs: Value,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FunctionCall {
    pub name: SmolStr,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Inst {
    pub kind: InstKind,
    pub ty: Ty,
}

/// Kinds of an instruction
#[derive(Debug, Clone, Eq, PartialEq, EnumAsInner)]
pub enum InstKind {
    /// A binary operaton, e.g. plus, divide
    Binary(BinaryInst),
    /// A call to another function.
    FunctionCall(FunctionCall),

    /// An assignment from another instruction or constant
    Assign(Value),
    /// A phi instruction
    Phi(BTreeMap<BBId, InstId>),
    /// A function parameter
    Param(usize),
    /// An unreachable value
    Dead,
}

impl InstKind {
    pub fn params_iter(&self) -> impl Iterator<Item = Value> + '_ {
        match self {
            InstKind::Binary(b) => VarIter::Two(b.lhs, b.rhs),
            InstKind::FunctionCall(f) => {
                VarIter::Iter(Box::new(f.params.iter().cloned()) as Box<dyn Iterator<Item = _>>)
            }
            InstKind::Assign(v) => VarIter::One(*v),
            InstKind::Phi(source) => {
                VarIter::Iter(Box::new(source.iter().map(|(_, &val)| val.into()))
                    as Box<dyn Iterator<Item = _>>)
            }
            InstKind::Param(_) => VarIter::None,
            InstKind::Dead => VarIter::None,
        }
    }

    pub fn param_op_iter(&self) -> impl Iterator<Item = InstId> + '_ {
        self.params_iter().filter_map(|x| x.get_inst())
    }
}

/// Represents a branch instruction.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Branch {
    /// Returns the given value.
    Return(Option<Value>),

    /// Jumps to the given target
    Jump(BBId),

    /// Conditional jump to the given target.
    ///
    /// `cond` must be a boolean or integer.
    CondJump { cond: Value, target: BBId },
}

// impl Default for Branch {
//     fn default() -> Self {
//         Self::Unreachable
//     }
// }

impl Branch {
    pub fn target_iter(&self) -> impl Iterator<Item = BBId> + '_ {
        match self {
            Branch::Return(_) => util::OptionIter::<BBId>::None,
            Branch::Jump(t) => util::OptionIter::One(*t),
            Branch::CondJump { target, .. } => util::OptionIter::One(*target),
            // Branch::TableJump { target, .. } => util::VarIter::Iter(target.iter().map(|t| t.bb)),
            // Branch::Unreachable => util::VarIter::None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Value {
    Dest(InstId),
    Imm(Immediate),
}

impl Value {
    pub fn get_imm(&self) -> Option<Immediate> {
        match self {
            Value::Dest(_) => None,
            Value::Imm(i) => Some(*i),
        }
    }

    pub fn get_inst(&self) -> Option<InstId> {
        match self {
            Value::Dest(o) => Some(*o),
            _ => None,
        }
    }
}

impl From<InstId> for Value {
    fn from(x: InstId) -> Self {
        Value::Dest(x)
    }
}

type Immediate = i64;
