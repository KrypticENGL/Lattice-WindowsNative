//! Refines relocation edges: a relocation inside a call or jump instruction
//! is a `Calls` edge, anything else stays `References`. x86-64 only.
//!
//! Compilers also hoist the target out of a loop: `mov r13, [rip+f@GOTPCREL]`
//! followed later by `call r13` (or MSVC's `mov rax, [__imp_f]; call rax`).
//! A RIP-relative load whose register is then called is treated as a call
//! site too. Tracking is per decoded segment and deliberately simple: any
//! write to the register drops it, and calls drop caller-saved registers.

use std::collections::HashMap;

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register};

/// Registers a call may clobber (Windows x64 and System V share these).
const VOLATILE: &[Register] = &[
    Register::RAX,
    Register::RCX,
    Register::RDX,
    Register::R8,
    Register::R9,
    Register::R10,
    Register::R11,
];

/// Byte spans of control-transfer instructions in one code section.
#[derive(Debug, Default)]
pub(crate) struct CallSites {
    /// Sorted, non-overlapping `[start, end)` instruction spans.
    spans: Vec<(u64, u64)>,
}

impl CallSites {
    /// Linear-sweep disassembly of `code`. Decoding restarts at every symbol
    /// start in `starts`, so data or padding between functions cannot
    /// desynchronise later functions.
    pub(crate) fn analyze(code: &[u8], starts: &[u64]) -> Self {
        let len = code.len() as u64;
        let mut cuts: Vec<u64> = starts.iter().copied().filter(|&s| s < len).collect();
        cuts.push(0);
        cuts.sort_unstable();
        cuts.dedup();

        let mut spans = Vec::new();
        for (i, &start) in cuts.iter().enumerate() {
            let end = cuts.get(i + 1).copied().unwrap_or(len);
            let (Ok(s), Ok(e)) = (usize::try_from(start), usize::try_from(end)) else {
                continue;
            };
            let Some(bytes) = code.get(s..e) else {
                continue;
            };
            let mut decoder = Decoder::with_ip(64, bytes, start, DecoderOptions::NONE);
            // Register -> span of the RIP-relative load that last set it.
            let mut loads: HashMap<Register, (u64, u64)> = HashMap::new();
            while decoder.can_decode() {
                let instr = decoder.decode();
                if instr.is_invalid() {
                    loads.clear();
                    continue;
                }
                let flow = instr.flow_control();
                if is_control_transfer(flow) {
                    spans.push((instr.ip(), instr.next_ip()));
                    if let Some(reg) = register_operand(&instr) {
                        if let Some(&load) = loads.get(&reg) {
                            spans.push(load);
                        }
                    }
                }
                if matches!(flow, FlowControl::Call | FlowControl::IndirectCall) {
                    for reg in VOLATILE {
                        loads.remove(reg);
                    }
                    continue;
                }
                if let Some(reg) = register_operand(&instr) {
                    let is_rip_load = instr.mnemonic() == Mnemonic::Mov
                        && instr.op_count() == 2
                        && instr.op1_kind() == OpKind::Memory
                        && instr.is_ip_rel_memory_operand();
                    if is_rip_load {
                        loads.insert(reg, (instr.ip(), instr.next_ip()));
                    } else {
                        loads.remove(&reg);
                    }
                }
            }
        }
        spans.sort_unstable();
        spans.dedup();
        Self { spans }
    }

    /// Whether `offset` lies inside a call or jump instruction.
    pub(crate) fn contains(&self, offset: u64) -> bool {
        let i = self.spans.partition_point(|&(start, _)| start <= offset);
        i.checked_sub(1)
            .and_then(|i| self.spans.get(i))
            .is_some_and(|&(_, end)| offset < end)
    }
}

/// The full 64-bit register in operand 0, if operand 0 is a register.
fn register_operand(instr: &Instruction) -> Option<Register> {
    (instr.op_count() > 0 && instr.op0_kind() == OpKind::Register)
        .then(|| instr.op0_register().full_register())
}

/// Calls, plus jumps: a relocated jump target leaves the function, which is
/// a tail call.
fn is_control_transfer(flow: FlowControl) -> bool {
    matches!(
        flow,
        FlowControl::Call
            | FlowControl::IndirectCall
            | FlowControl::UnconditionalBranch
            | FlowControl::ConditionalBranch
            | FlowControl::IndirectBranch
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_call_displacement_is_a_call() {
        // call rel32 ; ret
        let code = [0xE8, 0, 0, 0, 0, 0xC3];
        let sites = CallSites::analyze(&code, &[0]);
        assert!(sites.contains(1));
        assert!(!sites.contains(5));
    }

    #[test]
    fn rip_relative_lea_is_not_a_call() {
        // lea rax, [rip+0] ; call qword ptr [rip+0] ; jmp rel32
        let code = [
            0x48, 0x8D, 0x05, 0, 0, 0, 0, 0xFF, 0x15, 0, 0, 0, 0, 0xE9, 0, 0, 0, 0,
        ];
        let sites = CallSites::analyze(&code, &[0]);
        assert!(!sites.contains(3), "lea operand");
        assert!(sites.contains(9), "indirect call through memory");
        assert!(sites.contains(14), "tail jump");
    }

    #[test]
    fn decoding_restarts_at_symbol_starts() {
        // f: 0xCC padding byte that would misalign, then g: call rel32.
        let code = [0x0F, 0xE8, 0, 0, 0, 0];
        let sites = CallSites::analyze(&code, &[0, 1]);
        assert!(sites.contains(2));
    }

    #[test]
    fn hoisted_load_then_register_call_is_a_call() {
        // mov r13, [rip+0] ; xor ebp, ebp ; call r13
        let code = [0x4C, 0x8B, 0x2D, 0, 0, 0, 0, 0x31, 0xED, 0x41, 0xFF, 0xD5];
        let sites = CallSites::analyze(&code, &[0]);
        assert!(sites.contains(3));
    }

    #[test]
    fn overwritten_or_clobbered_load_is_not_a_call() {
        // mov rax, [rip+0] ; xor eax, eax ; call rax
        let overwritten = [0x48, 0x8B, 0x05, 0, 0, 0, 0, 0x31, 0xC0, 0xFF, 0xD0];
        assert!(!CallSites::analyze(&overwritten, &[0]).contains(3));
        // mov rax, [rip+0] ; call rel32 (clobbers rax) ; call rax
        let clobbered = [0x48, 0x8B, 0x05, 0, 0, 0, 0, 0xE8, 0, 0, 0, 0, 0xFF, 0xD0];
        assert!(!CallSites::analyze(&clobbered, &[0]).contains(3));
        // mov rax, [rip+0] ; ret  (the pointer is returned, not called)
        let returned = [0x48, 0x8B, 0x05, 0, 0, 0, 0, 0xC3];
        assert!(!CallSites::analyze(&returned, &[0]).contains(3));
    }

    #[test]
    fn garbage_and_empty_input_do_not_panic() {
        let _ = CallSites::analyze(&[], &[0, 5, u64::MAX]);
        let junk: Vec<u8> = (0..=255).collect();
        let _ = CallSites::analyze(&junk, &[3, 7, 300]);
    }
}
