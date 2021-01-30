pub mod err;

use azuki_syntax::{ast::*, visitor::AstVisitor};
use azuki_tac as tac;
use bit_set::BitSet;
use err::Error;
use std::{borrow::Borrow, collections::BTreeMap, ops::Deref};

use tac::{BasicBlock, BinaryInst, Branch, Inst, InstKind, OpRef, TacFunc, Ty, Value};

fn compile(tac: &Program) {}

struct FuncCompiler {
    builder: tac::builder::FuncBuilder,
}

fn empty_jump_target(bb_id: usize) -> tac::BranchTarget {
    tac::BranchTarget {
        bb: bb_id,
        params: BTreeMap::new(),
    }
}

// This implementation is the main tac-generation part.
//
// I try to use the method in https://pp.ipd.kit.edu/uploads/publikationen/braun13cc.pdf
// to directly generate SSA code from AST.
//
// Notes:
//
// - All basic blocks are marked as filled and sealed when its successor is created in another
//   visitor method. Any basic block that needs special treatments (e.g. late sealing in control
//   flows) should be managed within a single visitor method.
impl AstVisitor for FuncCompiler {
    type LExprResult = ();

    type ExprResult = Result<(Value, Ty), Error>;

    type TyResult = ();

    type StmtResult = Result<(), Error>;

    type ProgramResult = ();

    type FuncResult = ();

    fn visit_literal_expr(&mut self, _expr: &LiteralExpr) -> Self::ExprResult {
        match _expr.kind {
            LiteralKind::Integer(val) => Ok((Value::Imm(val as i64), Ty::Int)),
            LiteralKind::Float(_) => {
                todo!("implement float (or not)")
            }
            LiteralKind::String(_) => {
                todo!("Implement String")
            }
            LiteralKind::Char(ch) => Ok((Value::Imm(ch as i64), Ty::Int)),
        }
    }

    fn visit_binary_expr(&mut self, expr: &BinaryExpr) -> Self::ExprResult {
        let (lhsv, lhst) = self.visit_expr(&expr.lhs)?;
        let (rhsv, rhst) = self.visit_expr(&expr.rhs)?;

        assert_type_eq(&lhst, &rhst)?;

        let v = self.builder.insert_after_current_place(Inst {
            kind: InstKind::Binary(BinaryInst {
                op: match expr.op {
                    BinaryOp::Add => tac::BinaryOp::Add,
                    BinaryOp::Sub => tac::BinaryOp::Sub,
                    BinaryOp::Mul => tac::BinaryOp::Mul,
                    BinaryOp::Div => tac::BinaryOp::Div,
                    BinaryOp::Gt => tac::BinaryOp::Gt,
                    BinaryOp::Lt => tac::BinaryOp::Lt,
                    BinaryOp::Ge => tac::BinaryOp::Ge,
                    BinaryOp::Le => tac::BinaryOp::Le,
                    BinaryOp::Eq => tac::BinaryOp::Eq,
                    BinaryOp::Neq => tac::BinaryOp::Ne,
                },
                lhs: lhsv,
                rhs: rhsv,
            }),
            ty: lhst.clone(),
        });

        Ok((v.into(), lhst))
    }

    fn visit_unary_expr(&mut self, expr: &UnaryExpr) -> Self::ExprResult {
        let (v, t) = self.visit_expr(&expr.expr)?;

        match expr.op {
            UnaryOp::Neg => {
                let v = self.builder.insert_after_current_place(Inst {
                    kind: InstKind::Binary(BinaryInst {
                        op: tac::BinaryOp::Sub,
                        lhs: Value::Imm(0),
                        rhs: v,
                    }),
                    ty: t.clone(),
                });
                Ok((v.into(), t))
            }
            UnaryOp::Pos => Ok((v, t)),
        }
    }

    fn visit_as_expr(&mut self, expr: &AsExpr) -> Self::ExprResult {
        self.visit_expr(&expr.val)
    }

    fn visit_while_stmt(&mut self, stmt: &WhileStmt) -> Self::StmtResult {
        let cur_bb = self.builder.current_bb();
        let cond_bb = self.builder.new_bb();
        self.builder
            .add_branch(Branch::Jump(empty_jump_target(cond_bb)), cur_bb)
            .unwrap();

        self.builder.mark_sealed(cur_bb);
        self.builder.mark_filled(cur_bb);

        self.builder.set_current_bb(cond_bb).unwrap();
        let (cond, _cond_ty) = self.visit_expr(&stmt.cond)?;

        let loop_bb = self.builder.new_bb();
        let next_bb = self.builder.new_bb();

        self.builder.mark_filled(cond_bb);

        // cond_bb --> loop_bb
        //   \---> next_bb
        self.builder
            .add_branch(
                Branch::CondJump {
                    cond,
                    target: empty_jump_target(loop_bb),
                },
                cond_bb,
            )
            .unwrap();
        self.builder
            .add_branch(Branch::Jump(empty_jump_target(next_bb)), cond_bb)
            .unwrap();

        self.builder.set_current_bb(loop_bb).unwrap();
        self.visit_block_stmt(&stmt.body)?;
        let loop_end_bb = self.builder.current_bb();

        self.builder
            .add_branch(Branch::Jump(empty_jump_target(cond_bb)), loop_end_bb)
            .unwrap();

        self.builder.mark_sealed(loop_end_bb);
        self.builder.mark_filled(loop_end_bb);
        self.builder.mark_sealed(cond_bb);

        self.builder.set_current_bb(next_bb).unwrap();

        Ok(())
    }

    fn visit_if_stmt(&mut self, stmt: &IfStmt) -> Self::StmtResult {
        let expr_val = self.visit_expr(&stmt.cond)?;
        let last_bb = self.builder.current_bb();

        self.builder.mark_sealed(last_bb);
        self.builder.mark_sealed(last_bb);

        // Create if block
        let if_bb = self.builder.new_bb();

        // if -> if_bb
        self.builder
            .add_branch(
                Branch::CondJump {
                    cond: expr_val.0,
                    target: empty_jump_target(if_bb),
                },
                last_bb,
            )
            .unwrap();

        self.builder.set_current_bb(if_bb).unwrap();
        self.visit_block_stmt(&stmt.if_block)?;

        let if_end_bb = self.builder.current_bb();

        // The basic block after the if statement
        let next_bb = self.builder.new_bb();

        // Deal with else block
        let else_bbs = match &stmt.else_block {
            other @ IfElseBlock::Block(..) | other @ IfElseBlock::If(..) => {
                let else_bb = self.builder.new_bb();

                // if
                //  \--> else_bb
                self.builder
                    .add_branch(Branch::Jump(empty_jump_target(else_bb)), last_bb)
                    .unwrap();

                self.builder.set_current_bb(else_bb).unwrap();

                match other {
                    IfElseBlock::None => unreachable!(),
                    IfElseBlock::If(i) => self.visit_if_stmt(&i)?,
                    IfElseBlock::Block(b) => self.visit_block_stmt(&b)?,
                }

                Some((else_bb, self.builder.current_bb()))
            }
            azuki_syntax::ast::IfElseBlock::None => {
                // if
                //  \--> next_bb
                self.builder
                    .add_branch(Branch::Jump(empty_jump_target(next_bb)), last_bb)
                    .unwrap();
                None
            }
        };

        // if_end_bb -> next_bb
        self.builder
            .add_branch(Branch::Jump(empty_jump_target(next_bb)), if_end_bb)
            .unwrap();

        // else_end_bb -> next_bb
        if let Some((_, bb)) = else_bbs {
            self.builder
                .add_branch(Branch::Jump(empty_jump_target(next_bb)), bb)
                .unwrap();
        }

        self.builder.set_current_bb(next_bb).unwrap();
        Ok(())
    }

    fn visit_return_stmt(&mut self, stmt: &ReturnStmt) -> Self::StmtResult {
        let val = if let Some(val) = &stmt.val {
            Some(self.visit_expr(&val)?)
        } else {
            None
        };

        self.builder
            .add_branch(Branch::Return(val.map(|x| x.0)), self.builder.current_bb())
            .unwrap();

        let next_bb = self.builder.new_bb();
        self.builder.set_current_bb(next_bb).unwrap();

        Ok(())
    }

    fn visit_program(&mut self, program: &Program) -> Self::ProgramResult {
        for decl in &program.decls {
            self.visit_decl_stmt(decl);
        }
        for func in &program.funcs {
            self.visit_func(func);
        }
        todo!("Visit program")
    }

    fn visit_func(&mut self, func: &FuncStmt) -> Self::FuncResult {
        for param in &func.params {
            self.visit_func_param(param);
        }
        self.visit_block_stmt(&func.body);
        todo!("Visit function")
    }

    fn visit_func_param(&mut self, _param: &FuncParam) -> Self::StmtResult {
        todo!("Visit function param")
    }

    fn visit_ty(&mut self, _ty: &TyDef) -> Self::TyResult {
        todo!("Visit type")
    }

    fn visit_ident_expr(&mut self, _expr: &Ident) -> Self::ExprResult {
        todo!("visit")
    }

    fn visit_assign_expr(&mut self, expr: &AssignExpr) -> Self::ExprResult {
        self.visit_lexpr(&expr.lhs);
        self.visit_expr(&expr.rhs);
        todo!("visit")
    }

    fn visit_lexpr(&mut self, _expr: &Expr) -> Self::LExprResult {
        todo!("visit")
    }

    fn visit_call_expr(&mut self, expr: &CallExpr) -> Self::ExprResult {
        for subexpr in &expr.params {
            self.visit_expr(&subexpr);
        }
        todo!("visit")
    }

    fn visit_block_stmt(&mut self, stmt: &BlockStmt) -> Self::StmtResult {
        for substmt in &stmt.stmts {
            self.visit_stmt(substmt);
        }
        todo!("visit")
    }

    fn visit_expr_stmt(&mut self, stmt: &Expr) -> Self::StmtResult {
        self.visit_expr(stmt);
        todo!("visit")
    }

    fn visit_decl_stmt(&mut self, stmt: &DeclStmt) -> Self::StmtResult {
        self.visit_ty(&stmt.ty);
        if let Some(expr) = &stmt.val {
            self.visit_expr(expr);
        }
        todo!("visit")
    }

    fn visit_break_stmt(&mut self, _span: azuki_syntax::span::Span) -> Self::StmtResult {
        todo!("visit")
    }

    fn visit_continue_stmt(&mut self, _span: azuki_syntax::span::Span) -> Self::StmtResult {
        todo!("visit")
    }

    fn visit_empty_stmt(&mut self, _span: azuki_syntax::span::Span) -> Self::StmtResult {
        todo!("visit")
    }
}

fn assert_type_eq(lhs: &Ty, rhs: &Ty) -> Result<(), err::Error> {
    if lhs != rhs {
        return Err(Error::TypeMismatch {
            expected: lhs.clone(),
            found: rhs.clone(),
        });
    }
    Ok(())
}
