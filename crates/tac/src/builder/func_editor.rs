use petgraph::visit::EdgeRef;
use tinyvec::TinyVec;

use crate::{
    err::{Error, TacResult},
    BBId, BasicBlock, Branch, Inst, InstKind, OpRef, TacFunc, Ty,
};

use super::{SmallBBIdVec, SmallEdgeVec};

pub struct FuncEditor<'a> {
    pub func: &'a mut TacFunc,

    /// The basic block we're currently working on. Must be a valid basic block
    /// inside this function.
    current_bb: BBId,

    /// The instruction index we're currently working on. New instructions will
    /// be inserted before or after this instruction, depending on the function
    /// we use.
    ///
    /// **This value MUST refer to an instruction inside [`current_bb`](Self::current_bb).**
    /// **If this value is [`None`](Option::None), `current_bb` MUST be empty.**
    current_idx: Option<OpRef>,
}

impl<'a> FuncEditor<'a> {
    pub fn new(func: &'a mut TacFunc) -> FuncEditor<'a> {
        let starting_idx = func
            .basic_blocks
            .node_weight(func.starting_block)
            .unwrap()
            .head;
        let current_bb = func.starting_block;

        FuncEditor {
            func,
            current_bb,
            current_idx: starting_idx,
        }
    }

    pub fn set_type(&mut self, ty: Ty) {
        self.func.ty = ty;
    }

    /// Returns the current basic block this builder is working on.
    pub fn current_bb(&self) -> BBId {
        self.current_bb
    }

    /// Returns the current instruction this builder is working on. If
    /// [`current_bb`](Self::current_bb) is empty, returns [`None`](Option::None).
    pub fn current_idx(&self) -> Option<OpRef> {
        self.current_idx
    }

    /// Add an free-standing empty basic block into the function.
    pub fn new_bb(&mut self) -> BBId {
        self.func.basic_blocks.add_node(BasicBlock {
            jumps: vec![],
            head: None,
            tail: None,
        })
    }

    /// Set current basic block to `bb_id`. Also sets [`current_idx`](Self::current_idx)
    /// to the end of this basic block.
    ///
    /// Returns whether the position was **unchanged**.
    pub fn set_current_bb(&mut self, bb_id: BBId) -> TacResult<bool> {
        let bb = self
            .func
            .basic_blocks
            .node_weight(bb_id)
            .ok_or(Error::NoSuchBB(bb_id))?;
        let same_pos = bb_id == self.current_bb && bb.tail == self.current_idx;
        self.current_bb = bb_id;
        self.current_idx = bb.tail;
        Ok(same_pos)
    }

    /// Set current basic block to `bb_id`. Also sets [`current_idx`](Self::current_idx)
    /// to the start of this basic block.
    ///
    /// Returns whether the position was **unchanged**.
    pub fn set_current_bb_start(&mut self, bb_id: BBId) -> TacResult<bool> {
        let bb = self
            .func
            .basic_blocks
            .node_weight(bb_id)
            .ok_or(Error::NoSuchBB(bb_id))?;
        let same_pos = bb_id == self.current_bb && bb.head == self.current_idx;
        self.current_bb = bb_id;
        self.current_idx = bb.head;
        Ok(same_pos)
    }

    /// Sets current basic block and instruction position at the position of the
    /// given instruction.
    ///
    /// Returns whether the position was **unchanged**.
    pub fn set_position_at_instruction(&mut self, inst_idx: OpRef) -> TacResult<bool> {
        let inst = self.func.arena_get(inst_idx)?;
        let bb = inst.bb;
        let same_pos = bb == self.current_bb && Some(inst_idx) == self.current_idx;
        self.current_bb = bb;
        self.current_idx = Some(inst_idx);
        Ok(same_pos)
    }

    /// Insert the given instruction **after** the current place. Returns the index to
    /// the inserted instruction (and also the SSA value it's related to).
    ///
    /// If the current basic block is empty, the instruction is inserted as the
    /// only instruction of the basic block.
    pub fn insert_after_current_place(&mut self, inst: Inst) -> OpRef {
        let idx = self.func.tac_new(inst, self.current_bb());
        if let Some(cur_idx) = self.current_idx {
            self.func.tac_set_after(cur_idx, idx).unwrap();
            let bb = self
                .func
                .basic_blocks
                .node_weight_mut(self.current_bb)
                .unwrap();

            // reset tail pointer, since insertion might be at the end
            if bb.tail == Some(cur_idx) {
                bb.tail = Some(idx);
            }
        } else {
            let bb = self
                .func
                .basic_blocks
                .node_weight_mut(self.current_bb)
                .unwrap();
            bb.head = Some(idx);
            bb.tail = Some(idx);
        }
        self.current_idx = Some(idx);
        idx
    }

    /// Insert the given instruction **before** the current place. Returns the index to
    /// the inserted instruction (and also the SSA value it's related to).
    ///
    /// If the current basic block is empty, the instruction is inserted as the
    /// only instruction of the basic block.
    pub fn insert_before_current_place(&mut self, inst: Inst) -> OpRef {
        let idx = self.func.tac_new(inst, self.current_bb());
        if let Some(cur_idx) = self.current_idx {
            self.func.tac_set_before(cur_idx, idx).unwrap();
            let bb = self
                .func
                .basic_blocks
                .node_weight_mut(self.current_bb)
                .unwrap();

            // reset head pointer, since insertion might be at the start
            if bb.head == self.current_idx {
                bb.head = Some(idx);
            }
        } else {
            let bb = self
                .func
                .basic_blocks
                .node_weight_mut(self.current_bb)
                .unwrap();
            bb.head = Some(idx);
            bb.tail = Some(idx);
        }
        self.current_idx = Some(idx);
        idx
    }

    /// Insert the given instruction at the **end** of the given basic block.
    pub fn insert_at_end_of(&mut self, inst: Inst, bb_id: BBId) -> TacResult<OpRef> {
        let curr_bb = self.current_bb;
        let curr_idx = self.current_idx;
        let same_pos = self.set_current_bb(bb_id)?;
        let insert_pos = self.insert_after_current_place(inst);
        if !same_pos {
            self.current_bb = curr_bb;
            self.current_idx = curr_idx;
        }
        Ok(insert_pos)
    }

    /// Insert the given instruction at the **start** of the given basic block.
    pub fn insert_at_start_of(&mut self, inst: Inst, bb_id: BBId) -> TacResult<OpRef> {
        let curr_bb = self.current_bb;
        let curr_idx = self.current_idx;
        let same_pos = self.set_current_bb_start(bb_id)?;
        let insert_pos = self.insert_before_current_place(inst);
        if !same_pos {
            self.current_bb = curr_bb;
            self.current_idx = curr_idx;
        }
        Ok(insert_pos)
    }

    /// Add a branching instruction to the given basic block's jump instruction list.
    pub fn add_branch(&mut self, inst: Branch, bb_id: BBId) -> TacResult<()> {
        if self.func.basic_blocks.node_weight(bb_id).is_none() {
            return Err(Error::NoSuchBB(bb_id));
        }

        for target in inst.iter() {
            self.func.basic_blocks.add_edge(bb_id, target, ());
        }

        let bb = self.func.basic_blocks.node_weight_mut(bb_id).unwrap();

        bb.jumps.push(inst);

        Ok(())
    }

    /// Modifies the branching instructions of a basic block. Recalculates successors of this
    /// basic block after the modification completes.
    pub fn modify_branch<F: FnOnce(&mut Vec<Branch>)>(
        &mut self,
        bb_id: BBId,
        f: F,
    ) -> TacResult<()> {
        for edge in self.succ_edge_of_bb(bb_id) {
            self.func.basic_blocks.remove_edge(edge);
        }

        let bb = self
            .func
            .basic_blocks
            .node_weight_mut(bb_id)
            .ok_or(Error::NoSuchBB(bb_id))?;

        f(&mut bb.jumps);

        for target in bb
            .jumps
            .iter()
            .flat_map(|x| x.iter())
            .collect::<TinyVec<[_; 16]>>()
        {
            self.func.basic_blocks.add_edge(bb_id, target, ());
        }

        Ok(())
    }

    /// Returns an iterator of all predecessors of a basic block.
    ///
    /// The return type is to make the borrow checker happy.
    pub fn pred_of_bb(&self, bb_id: BBId) -> SmallBBIdVec {
        self.func
            .basic_blocks
            .neighbors_directed(bb_id, petgraph::Direction::Incoming)
            .collect()
    }

    /// Returns an iterator of all successors of a basic block.
    pub fn succ_of_bb(&self, bb_id: BBId) -> SmallBBIdVec {
        self.func
            .basic_blocks
            .neighbors_directed(bb_id, petgraph::Direction::Outgoing)
            .collect()
    }

    /// Returns an iterator of all predecessors of a basic block.
    ///
    /// The return type is to make the borrow checker happy.
    pub fn pred_edge_of_bb(&self, bb_id: BBId) -> SmallEdgeVec {
        self.func
            .basic_blocks
            .edges_directed(bb_id, petgraph::Direction::Incoming)
            .map(|e| e.id())
            .collect()
    }

    /// Returns an iterator of all successors of a basic block.
    pub fn succ_edge_of_bb(&self, bb_id: BBId) -> SmallEdgeVec {
        self.func
            .basic_blocks
            .edges_directed(bb_id, petgraph::Direction::Outgoing)
            .map(|e| e.id())
            .collect()
    }

    pub fn insert_param(&mut self, bb_id: BBId, ty: Ty) -> Result<OpRef, Error> {
        self.insert_at_start_of(
            Inst {
                kind: InstKind::Param,
                ty,
            },
            bb_id,
        )
    }
}
