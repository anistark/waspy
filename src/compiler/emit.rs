//! The function body every code generator writes into.
//!
//! Runtime helpers are written straight into a `wasm_encoder::Function`. A
//! function of the program is written into a body that is assembled once it
//! is complete, because what its prologue has to set up is only known then:
//! how many of its locals hold references the collector must see, and whether
//! it leaves anything on the root stack at all (see `compiler::gc`).
//!
//! Three things are done here, at the one place every instruction passes
//! through, so that no code generator has to remember them:
//!
//! - A store to a local that can hold a reference is followed by a store of
//!   the same word into the function's frame on the root stack. A reference
//!   held only in a WASM local is invisible to the collector, which would
//!   free what it points at.
//! - A `return` becomes a branch to the function's one exit, where the frame
//!   is popped.
//! - Calls into the runtime are counted, which is how a statement knows it
//!   may have left temporaries on the root stack to drop.

use crate::compiler::gc::{RS_BASE_GLOBAL, RS_TOP_GLOBAL};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_encoder::{BlockType, Instruction, MemArg, ValType};

/// A function under construction.
pub struct Function {
    inner: wasm_encoder::Function,
    roots: Option<Roots>,
}

/// What a function of the program tracks while its body is written.
struct Roots {
    /// The locals that can hold a reference: every named local that is one
    /// word and is not typed as a number.
    eligible: HashSet<u32>,
    /// The frame slot of each such local that has been stored to.
    slots: HashMap<u32, u32>,
    /// The local holding this activation's frame, an offset into the root
    /// stack.
    frame_local: u32,
    /// Blocks open at this point of the body, which is how far a `return`
    /// has to branch.
    depth: u32,
    /// Function indices from here up are the runtime's.
    first_runtime_fn: u32,
    /// The functions of the program that return a reference, which each
    /// leaves on the root stack for its caller.
    returns_reference: Rc<HashSet<u32>>,
    /// Calls written so far that can leave something on the root stack: one
    /// into the runtime, or to a function returning a reference.
    runtime_calls: u32,
}

/// What a finished body needs from its prologue and exit.
pub struct Frame {
    /// The body's instructions, without a locals declaration or final `end`.
    pub body: Vec<u8>,
    /// Frame slots the body stores into.
    pub slots: u32,
    /// Whether the body called into the runtime, and so may have left
    /// temporaries on the root stack.
    pub runtime_calls: bool,
}

impl Function {
    /// A runtime helper, or a scratch body whose code is thrown away.
    pub fn new<L>(locals: L) -> Self
    where
        L: IntoIterator<Item = (u32, ValType)>,
        L::IntoIter: ExactSizeIterator,
    {
        Function {
            inner: wasm_encoder::Function::new(locals),
            roots: None,
        }
    }

    /// The body of a function of the program. `eligible` are the locals that
    /// can hold a reference, `frame_local` the local its frame offset is kept
    /// in, function indices from `first_runtime_fn` up are the runtime's, and
    /// `returns_reference` are the program's functions that return one.
    pub fn user(
        eligible: HashSet<u32>,
        frame_local: u32,
        first_runtime_fn: u32,
        returns_reference: Rc<HashSet<u32>>,
    ) -> Self {
        Function {
            inner: wasm_encoder::Function::new([]),
            roots: Some(Roots {
                eligible,
                slots: HashMap::new(),
                frame_local,
                depth: 0,
                first_runtime_fn,
                returns_reference,
                runtime_calls: 0,
            }),
        }
    }

    /// A finished function around `inner`.
    pub fn from_wasm(inner: wasm_encoder::Function) -> Self {
        Function { inner, roots: None }
    }

    pub fn as_wasm(&self) -> &wasm_encoder::Function {
        &self.inner
    }

    /// The local `index` holds a reference from here on: code generation
    /// has just learned its type.
    pub fn make_eligible(&mut self, index: u32) {
        if let Some(roots) = &mut self.roots {
            roots.eligible.insert(index);
        }
    }

    /// Store a number (an int or a bool, never a reference) into the local
    /// `index`. A local that has held a reference has a frame slot, which
    /// then takes the number too, so nothing stale is left there to keep an
    /// object alive; one that never has needs nothing mirrored, whatever its
    /// type was taken to be when the function began.
    pub fn set_number_local(&mut self, index: u32) {
        let mirrored = self
            .roots
            .as_ref()
            .is_some_and(|roots| roots.slots.contains_key(&index));
        if mirrored {
            self.instruction(&Instruction::LocalSet(index));
        } else {
            self.inner.instruction(&Instruction::LocalSet(index));
        }
    }

    /// Whether a store to the local `index` is mirrored on the root stack.
    pub fn is_eligible(&self, index: u32) -> bool {
        match &self.roots {
            Some(roots) => roots.eligible.contains(&index),
            None => true,
        }
    }

    /// Calls written so far that can leave something on the root stack. A
    /// statement compares this before and after its own code.
    pub fn runtime_calls(&self) -> u32 {
        self.roots.as_ref().map_or(0, |roots| roots.runtime_calls)
    }

    /// Count something written that works the root stack without a call.
    pub fn note_root_use(&mut self) {
        if let Some(roots) = &mut self.roots {
            roots.runtime_calls += 1;
        }
    }

    pub fn instruction(&mut self, instruction: &Instruction) -> &mut Self {
        let Some(roots) = &mut self.roots else {
            self.inner.instruction(instruction);
            return self;
        };
        match instruction {
            Instruction::LocalSet(index) | Instruction::LocalTee(index)
                if roots.eligible.contains(index) =>
            {
                let next = roots.slots.len() as u32;
                let slot = *roots.slots.entry(*index).or_insert(next);
                self.inner.instruction(instruction);
                self.inner
                    .instruction(&Instruction::GlobalGet(RS_BASE_GLOBAL));
                self.inner
                    .instruction(&Instruction::LocalGet(roots.frame_local));
                self.inner.instruction(&Instruction::I32Add);
                self.inner.instruction(&Instruction::LocalGet(*index));
                self.inner.instruction(&Instruction::I32Store(MemArg {
                    offset: (slot * 4) as u64,
                    align: 2,
                    memory_index: 0,
                }));
            }
            Instruction::Return => {
                self.inner.instruction(&Instruction::Br(roots.depth));
            }
            Instruction::Block(_) | Instruction::Loop(_) | Instruction::If(_) => {
                roots.depth += 1;
                self.inner.instruction(instruction);
            }
            Instruction::End => {
                roots.depth = roots.depth.saturating_sub(1);
                self.inner.instruction(instruction);
            }
            Instruction::Call(index) => {
                if *index >= roots.first_runtime_fn || roots.returns_reference.contains(index) {
                    roots.runtime_calls += 1;
                }
                self.inner.instruction(instruction);
            }
            // A closure or a method reached through the table: what it
            // returns is not known here.
            Instruction::CallIndirect { .. } => {
                roots.runtime_calls += 1;
                self.inner.instruction(instruction);
            }
            _ => {
                self.inner.instruction(instruction);
            }
        }
        self
    }

    /// The finished body of a function of the program.
    pub fn into_frame(self) -> Frame {
        let roots = self
            .roots
            .expect("only a function of the program has a frame");
        // The body was written with no locals declared, which encodes as one
        // byte ahead of the instructions.
        let mut body = self.inner.into_raw_body();
        body.remove(0);
        Frame {
            body,
            slots: roots.slots.len() as u32,
            runtime_calls: roots.runtime_calls > 0,
        }
    }
}

/// Assemble a function of the program from its body.
///
/// `locals` are its declared locals and `result` its result type. `prologue`
/// writes what runs before the body and `exit` what runs after it, on every
/// path out: the body's result is on the stack there, and must still be when
/// `exit` is done.
pub fn assemble(
    locals: Vec<(u32, ValType)>,
    result: ValType,
    body: &[u8],
    prologue: impl FnOnce(&mut wasm_encoder::Function),
    exit: impl FnOnce(&mut wasm_encoder::Function),
) -> Function {
    let mut function = wasm_encoder::Function::new(locals);
    prologue(&mut function);
    function.instruction(&Instruction::Block(BlockType::Result(result)));
    function.raw(body.iter().copied());
    function.instruction(&Instruction::End);
    exit(&mut function);
    function.instruction(&Instruction::End);
    Function::from_wasm(function)
}

/// Drop the temporaries a statement left on the root stack: everything above
/// the frame's slots, which `temps_local` marks.
pub fn emit_drop_temporaries(func: &mut Function, temps_local: u32) {
    func.instruction(&Instruction::LocalGet(temps_local));
    func.instruction(&Instruction::GlobalSet(RS_TOP_GLOBAL));
}
