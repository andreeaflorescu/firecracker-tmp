// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

extern crate libc;

extern crate logger;
extern crate sys_util;

use self::logger::{Metric, METRICS};
use std::collections::HashMap;
use std::result::Result;

/// Integer values for the level of seccomp filtering used.
/// See `struct SeccompLevel` for more information about the different levels.
pub const SECCOMP_LEVEL_ADVANCED: u32 = 2;
pub const SECCOMP_LEVEL_BASIC: u32 = 1;
pub const SECCOMP_LEVEL_NONE: u32 = 0;

/// Maximum number of instructions that a BPF program can have.
const BPF_MAX_LEN: usize = 4096;

/// BPF Instruction classes.
/// See /usr/include/linux/bpf_common.h .
const BPF_LD: u16 = 0x00;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;

/// BPF ld/ldx fields
/// See /usr/include/linux/bpf_common.h .
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;

/// BPF alu fields.
/// See /usr/include/linux/bpf_common.h .
const BPF_AND: u16 = 0x50;

/// BPF jmp fields.
/// See /usr/include/linux/bpf_common.h .
const BPF_JA: u16 = 0x00;
const BPF_JEQ: u16 = 0x10;
const BPF_JGT: u16 = 0x20;
const BPF_JGE: u16 = 0x30;
const BPF_K: u16 = 0x00;

/// Return codes for BPF programs.
///  See /usr/include/linux/seccomp.h .
const SECCOMP_RET_ALLOW: u32 = 0x7fff0000;
const SECCOMP_RET_ERRNO: u32 = 0x00050000;
const SECCOMP_RET_KILL: u32 = 0x00000000;
const SECCOMP_RET_LOG: u32 = 0x7ffc0000;
const SECCOMP_RET_TRACE: u32 = 0x7ff00000;
const SECCOMP_RET_TRAP: u32 = 0x00030000;
const SECCOMP_RET_MASK: u32 = 0x0000ffff;

/// x86_64 architecture identifier.
/// See /usr/include/linux/audit.h .
/// Defined as:
/// `#define AUDIT_ARCH_X86_64	(EM_X86_64|__AUDIT_ARCH_64BIT|__AUDIT_ARCH_LE)`
const AUDIT_ARCH_X86_64: u32 = 62 | 0x80000000 | 0x40000000;

/// The offset of `si_syscall` (offending syscall identifier) within the siginfo structure
/// expressed as an `(u)int*`.
/// Offset `6` for an `i32` field means that the needed information is located at `6 * sizeof(i32)`.
/// See /usr/include/linux/signal.h for the C struct definition.
/// See https://github.com/rust-lang/libc/issues/716 for why the offset is different in Rust.
const SI_OFF_SYSCALL: isize = 6;

/// The maximum number of a syscall argument.
/// A syscall can have at most 6 arguments.
/// Arguments are numbered from 0 to 5.
const ARG_NUMBER_MAX: u8 = 5;

/// The maximum number of BPF statements that a condition will be translated into.
const CONDITION_MAX_LEN: u16 = 6;

/// `struct seccomp_data` offsets and sizes of fields in bytes:
///
/// ```c
/// struct seccomp_data {
///     int nr;
///     __u32 arch;
///     __u64 instruction_pointer;
///     __u64 args[6];
/// };
/// ```
const SECCOMP_DATA_NR_OFFSET: u8 = 0;
const SECCOMP_DATA_ARGS_OFFSET: u8 = 16;
const SECCOMP_DATA_ARG_SIZE: u8 = 8;

/// Specifies the type of seccomp filtering used.
pub enum SeccompLevel<'a> {
    /// Seccomp filtering by analysing syscall number and argument values of syscall.
    Advanced(SeccompFilterContext),
    /// Seccomp filtering by analysing syscall number.
    Basic(&'a [i64]),
    /// No seccomp filtering.
    None,
}

/// Seccomp errors.
#[derive(Debug)]
pub enum Error {
    /// Filter context that when translated into BPF code, exceeds the maximum number of
    /// instructions that a BPF program can have.
    ContextTooLarge,
    /// Attempting to add an empty vector of rules to the rule chain of a syscall.
    EmptyRulesVector,
    /// Argument number that exceeds the maximum value.
    InvalidArgumentNumber,
}

/// Comparison to perform when matching a condition.
#[derive(PartialEq)]
pub enum SeccompCmpOp {
    /// Argument value is equal to the specified value.
    Eq,
    /// Argument value is greater than or equal to the specified value.
    Ge,
    /// Argument value is greater than specified value.
    Gt,
    /// Argument value is less than or equal to the specified value.
    Le,
    /// Argument value is less than specified value.
    Lt,
    /// Masked bits of argument value are equal to masked bits of specified value.
    MaskedEq(u64),
    /// Argument value is not equal to specified value.
    Ne,
}

/// Condition that syscall must match in order to satisfy a rule.
pub struct SeccompCondition {
    /// Number of the argument value that is to be compared.
    arg_number: u8,
    /// Comparison to perform.
    operator: SeccompCmpOp,
    /// The value that will be compared with the argument value.
    value: u64,
}

/// Actions that `seccomp` can apply to process calling a syscall.
pub enum SeccompAction {
    /// Allows syscall.
    Allow,
    /// Returns from syscall with specified error number.
    Errno(u32),
    /// Kills calling process.
    Kill,
    /// Same as allow but logs call.
    Log,
    /// Notifies tracing process of the caller with respective number.
    Trace(u32),
    /// Sends `SIGSYS` to the calling process.
    Trap,
}

/// Rule that `seccomp` attempts to match for a syscall.
///
/// If all conditions match then rule gets matched.
/// The action of the first rule that matches will be applied to the calling process.
/// If no rule matches the default action is applied.
pub struct SeccompRule {
    /// Conditions of rule that need to match in order for the rule to get matched.
    conditions: Vec<SeccompCondition>,
    /// Action applied to calling process if rule gets matched.
    action: SeccompAction,
}

/// Filter context containing rules assigned to syscall numbers.
pub struct SeccompFilterContext {
    /// Hash map, mapping a priority and a chain of rules to a syscall number.
    rules: HashMap<i64, (i64, Vec<SeccompRule>)>,
    /// Default action to apply to syscall numbers that do not exist in the hash map.
    default_action: SeccompAction,
}

/// BPF instruction structure definition.
/// See /usr/include/linux/filter.h .
#[repr(C)]
#[derive(Debug, PartialEq)]
struct sock_filter {
    pub code: ::std::os::raw::c_ushort,
    pub jt: ::std::os::raw::c_uchar,
    pub jf: ::std::os::raw::c_uchar,
    pub k: ::std::os::raw::c_uint,
}

/// BPF structure definition for filter array.
/// See /usr/include/linux/filter.h .
#[repr(C)]
#[derive(Debug)]
struct sock_fprog {
    pub len: ::std::os::raw::c_ushort,
    pub filter: *const sock_filter,
}

impl SeccompCondition {
    /// Creates a new `SeccompCondition`.
    pub fn new(arg_number: u8, operator: SeccompCmpOp, value: u64) -> Result<Self, Error> {
        // Checks that the given argument number is valid.
        if arg_number > ARG_NUMBER_MAX {
            return Err(Error::InvalidArgumentNumber);
        }

        Ok(Self {
            arg_number,
            operator,
            value,
        })
    }

    /// Helper method.
    /// Returns most significant half, least significant half of the `value` field of
    /// `SeccompCondition`, also returns the offsets of the most significant and least significant
    /// half of the argument specified by `arg_number` relative to `struct seccomp_data` passed to
    /// the BPF program by the kernel.
    fn value_segments(&self) -> (u32, u32, u8, u8) {
        // Splits the specified value into its most significant and least significant halves.
        let (msb, lsb) = ((self.value >> 32) as u32, self.value as u32);

        // Offset to the argument specified by `arg_number`.
        let arg_offset = SECCOMP_DATA_ARGS_OFFSET + self.arg_number * SECCOMP_DATA_ARG_SIZE;

        // Extracts offsets of most significant and least significant halves of argument.
        let (msb_offset, lsb_offset) = {
            #[cfg(target_endian = "big")]
            {
                (arg_offset, arg_offset + SECCOMP_DATA_ARG_SIZE / 2)
            }
            #[cfg(target_endian = "little")]
            {
                (arg_offset + SECCOMP_DATA_ARG_SIZE / 2, arg_offset)
            }
        };

        (msb, lsb, msb_offset, lsb_offset)
    }

    /// Helper methods, translating conditions into BPF statements, based on the operator of the
    /// condition.
    ///
    /// The `offset` parameter is a given jump offset to the start of the next rule. The jump is
    /// performed if the condition fails and thus the current rule does not match so `seccomp` tries
    /// to match the next rule by jumping out of the current rule.
    ///
    /// In case the condition is part of the last rule, the jump offset is to the default action of
    /// respective context.
    ///
    /// The most significant and least significant halves of the argument value are compared
    /// separately since the BPF operand and accumulator are 4 bytes whereas an argument value is 8.
    fn into_eq_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, offset + 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, lsb, 0, offset),
        ]
    }

    fn into_ge_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, msb, 3, 0),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, offset + 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGE + BPF_K, lsb, 0, offset),
        ]
    }

    fn into_gt_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, msb, 3, 0),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, offset + 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, lsb, 0, offset),
        ]
    }

    fn into_le_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, msb, offset + 3, 0),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, lsb, offset, 0),
        ]
    }

    fn into_lt_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGT + BPF_K, msb, offset + 3, 0),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JGE + BPF_K, lsb, offset, 0),
        ]
    }

    fn into_masked_eq_bpf(self, offset: u8, mask: u64) -> Vec<sock_filter> {
        let (_, _, msb_offset, lsb_offset) = self.value_segments();
        let masked_value = self.value & mask;
        let (msb, lsb) = ((masked_value >> 32) as u32, masked_value as u32);
        let (mask_msb, mask_lsb) = ((mask >> 32) as u32, mask as u32);

        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_STMT(BPF_ALU + BPF_AND + BPF_K, mask_msb),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, offset + 3),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_STMT(BPF_ALU + BPF_AND + BPF_K, mask_lsb),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, lsb, 0, offset),
        ]
    }

    fn into_ne_bpf(self, offset: u8) -> Vec<sock_filter> {
        let (msb, lsb, msb_offset, lsb_offset) = self.value_segments();
        vec![
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, msb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, msb, 0, 2),
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, lsb_offset as u32),
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, lsb, offset, 0),
        ]
    }

    /// Translates `SeccompCondition` into BPF statements.
    fn into_bpf(self, offset: u8) -> Vec<sock_filter> {
        let result = match self.operator {
            SeccompCmpOp::Eq => self.into_eq_bpf(offset),
            SeccompCmpOp::Ge => self.into_ge_bpf(offset),
            SeccompCmpOp::Gt => self.into_gt_bpf(offset),
            SeccompCmpOp::Le => self.into_le_bpf(offset),
            SeccompCmpOp::Lt => self.into_lt_bpf(offset),
            SeccompCmpOp::MaskedEq(mask) => self.into_masked_eq_bpf(offset, mask),
            SeccompCmpOp::Ne => self.into_ne_bpf(offset),
        };

        // Regression testing that the `CONDITION_MAX_LEN` constant was properly updated.
        assert!(result.len() <= CONDITION_MAX_LEN as usize);

        result
    }
}

impl From<SeccompAction> for u32 {
    /// Return codes of the BPF program for each action.
    fn from(action: SeccompAction) -> Self {
        match action {
            SeccompAction::Allow => SECCOMP_RET_ALLOW,
            SeccompAction::Errno(x) => SECCOMP_RET_ERRNO | (x & SECCOMP_RET_MASK),
            SeccompAction::Kill => SECCOMP_RET_KILL,
            SeccompAction::Log => SECCOMP_RET_LOG,
            SeccompAction::Trace(x) => SECCOMP_RET_TRACE | (x & SECCOMP_RET_MASK),
            SeccompAction::Trap => SECCOMP_RET_TRAP,
        }
    }
}

impl SeccompRule {
    /// Creates a new rule, rules with 0 conditions always match.
    pub fn new(conditions: Vec<SeccompCondition>, action: SeccompAction) -> Self {
        Self { conditions, action }
    }

    /// Translates a rule into BPF statements.
    ///
    /// Each rule starts with 2 jump statements:
    /// - The first jump enters the rule, attempting a match.
    /// - The second jump points to the end of the rule chain for one syscall, into the rule chain
    ///   for the next syscall or the default action if the current syscall is the last one. It
    ///   essentially jumps out of the current rule chain.
    fn into_bpf(self) -> Vec<sock_filter> {
        // Rule is built backwards, last statement is the action of the rule.
        // The offset to the next rule is 1.
        let mut accumulator = Vec::with_capacity(
            self.conditions.len()
                + ((self.conditions.len() * CONDITION_MAX_LEN as usize) / ::std::u8::MAX as usize)
                + 1,
        );
        let mut rule_len = 1;
        let mut offset = 1;
        accumulator.push(vec![BPF_STMT(BPF_RET + BPF_K, u32::from(self.action))]);

        // Conditions are translated into BPF statements and prepended to the rule.
        self.conditions.into_iter().for_each(|condition| {
            SeccompRule::append_condition(condition, &mut accumulator, &mut rule_len, &mut offset)
        });

        // The two initial jump statements are prepended to the rule.
        let rule_jumps = vec![
            BPF_STMT(BPF_JMP + BPF_JA, 1),
            BPF_STMT(BPF_JMP + BPF_JA, offset as u32 + 1),
        ];
        rule_len += rule_jumps.len();
        accumulator.push(rule_jumps);

        // Finally, builds the translated rule by consuming the accumulator.
        let mut result = Vec::with_capacity(rule_len);
        accumulator
            .into_iter()
            .rev()
            .for_each(|mut instructions| result.append(&mut instructions));

        result
    }

    /// Helper function.
    /// Appends a condition of the rule to an accumulator, updating the length of the rule and
    /// offset to the next rule.
    fn append_condition(
        condition: SeccompCondition,
        accumulator: &mut Vec<Vec<sock_filter>>,
        rule_len: &mut usize,
        offset: &mut u8,
    ) {
        // Tries to detect whether prepending the current condition will produce an unjumpable
        // offset (since BPF jumps are a maximum of 255 instructions).
        if *offset as u16 + CONDITION_MAX_LEN + 1 > ::std::u8::MAX as u16 {
            // If that is the case, three additional helper jumps are prepended and the offset
            // is reset to 1.
            //
            // - The first jump continues the evaluation of the condition chain by jumping to
            // the next condition or the action of the rule if the last condition was matched.
            // - The second, jumps out of the rule, to the next rule or the default action of
            // the context in case of the last rule in the rule chain of a syscall.
            // - The third jumps out of the rule chain of the syscall, to the rule chain of the
            // next syscall number to be checked or the default action of the context in the
            // case of the last rule chain.
            let helper_jumps = vec![
                BPF_STMT(BPF_JMP + BPF_JA, 2),
                BPF_STMT(BPF_JMP + BPF_JA, *offset as u32 + 1),
                BPF_STMT(BPF_JMP + BPF_JA, *offset as u32 + 1),
            ];
            *rule_len += helper_jumps.len();
            accumulator.push(helper_jumps);
            *offset = 1;
        }

        let condition = condition.into_bpf(*offset);
        *rule_len += condition.len();
        *offset += condition.len() as u8;
        accumulator.push(condition);
    }
}

impl SeccompFilterContext {
    /// Creates a new filter context
    pub fn new(
        rules: HashMap<i64, (i64, Vec<SeccompRule>)>,
        default_action: SeccompAction,
    ) -> Result<Self, Error> {
        // All inserted syscalls must have at least one rule, otherwise BPF code will break.
        for (_, value) in rules.iter() {
            if value.1.len() == 0 {
                return Err(Error::EmptyRulesVector);
            }
        }

        Ok(Self {
            rules,
            default_action,
        })
    }

    /// Adds a rule to a syscall number in the filter context.
    pub fn add_rule(
        &mut self,
        syscall_number: i64,
        default_priority: Option<i64>,
        rule: SeccompRule,
    ) {
        self.rules
            .entry(syscall_number)
            .or_insert_with(|| (default_priority.unwrap_or(0), vec![]))
            .1
            .push(rule);
    }

    /// Adds rules to a syscall number in the filter context.
    pub fn add_rules(
        &mut self,
        syscall_number: i64,
        default_priority: Option<i64>,
        mut rules: Vec<SeccompRule>,
    ) -> Result<(), Error> {
        // All inserted syscalls must have at least one rule, otherwise BPF code will break.
        if rules.len() == 0 {
            return Err(Error::EmptyRulesVector);
        }

        self.rules
            .entry(syscall_number)
            .or_insert_with(|| (default_priority.unwrap_or(0), vec![]))
            .1
            .append(&mut rules);

        Ok(())
    }

    /// Translates filter context into BPF instructions.
    fn into_bpf(self) -> Result<Vec<sock_filter>, Error> {
        // The called syscall number is loaded.
        let mut accumulator = Vec::with_capacity(1);
        let mut context_len = 1;
        accumulator.push(vec![BPF_STMT(
            BPF_LD + BPF_W + BPF_ABS,
            SECCOMP_DATA_NR_OFFSET as u32,
        )]);

        // Orders syscalls by priority, the highest number represents the highest priority.
        let mut iter = {
            let mut vec: Vec<_> = self.rules.into_iter().collect();
            accumulator.reserve_exact(vec.len() + 1);

            // (syscall_number, (priority, rules)), thus .1 is (priority, rules), (.1).0 is
            // priority.
            vec.sort_by(|a, b| (a.1).0.cmp(&(b.1).0).reverse());

            // Gets rid of priorities since syscalls were ordered.
            vec.into_iter().map(|(a, (_, b))| (a, b))
        };

        // For each syscall adds its rule chain to the context.
        let default_action = u32::from(self.default_action);
        iter.try_for_each(|(syscall_number, chain)| {
            SeccompFilterContext::append_syscall_chain(
                syscall_number,
                chain,
                default_action,
                &mut accumulator,
                &mut context_len,
            )
        })?;

        // The default action is once again appended, it is reached if all syscall number
        // comparisons fail.
        context_len += 1;
        accumulator.push(vec![BPF_STMT(BPF_RET + BPF_K, default_action)]);

        // Finally, builds the translated context by consuming the accumulator.
        let mut result = Vec::with_capacity(context_len);
        accumulator
            .into_iter()
            .for_each(|mut instructions| result.append(&mut instructions));

        Ok(result)
    }

    /// Helper function.
    /// Appends a chain of the context to an accumulator, updating the length of the context.
    fn append_syscall_chain(
        syscall_number: i64,
        chain: Vec<SeccompRule>,
        default_action: u32,
        accumulator: &mut Vec<Vec<sock_filter>>,
        context_len: &mut usize,
    ) -> Result<(), Error> {
        // The rules of the chain are translated into BPF statements.
        let chain: Vec<_> = chain.into_iter().map(|rule| rule.into_bpf()).collect();
        let chain_len = chain.iter().map(|rule| rule.len()).fold(0, |a, b| a + b);

        // The chain starts with a comparison checking the loaded syscall number against the
        // syscall number of the chain.
        let mut built_syscall = Vec::with_capacity(1 + chain_len + 1);
        built_syscall.push(BPF_JUMP(
            BPF_JMP + BPF_JEQ + BPF_K,
            syscall_number as u32,
            0,
            1,
        ));

        // The rules of the chain are appended.
        chain
            .into_iter()
            .for_each(|mut rule| built_syscall.append(&mut rule));

        // The default action is appended, if the syscall number comparison matched and then all
        // rules fail to match, the default action is reached.
        built_syscall.push(BPF_STMT(BPF_RET + BPF_K, default_action));

        // The chain is appended to the result.
        *context_len += built_syscall.len();
        accumulator.push(built_syscall);

        // BPF programs are limited to 4096 statements.
        if *context_len >= BPF_MAX_LEN {
            return Err(Error::ContextTooLarge);
        }

        Ok(())
    }
}

/// Builds the array of filter instructions and sends them to the kernel.
pub fn setup_seccomp(level: SeccompLevel) -> Result<(), i32> {
    let mut filters = Vec::new();

    filters.extend(VALIDATE_ARCHITECTURE());

    // Load filters according to specified filter level.
    match level {
        SeccompLevel::Advanced(context) => {
            filters.extend(context.into_bpf().map_err(|_| libc::EINVAL)?);
        }
        SeccompLevel::Basic(allowed_syscalls) => {
            filters.extend(EXAMINE_SYSCALL());
            for &syscall in allowed_syscalls {
                filters.extend(ALLOW_SYSCALL(syscall));
            }
            filters.extend(SIGNAL_PROCESS());
        }
        SeccompLevel::None => {
            return Ok(());
        }
    }

    unsafe {
        {
            let rc = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            if rc != 0 {
                return Err(*libc::__errno_location());
            }
        }

        let filter = sock_fprog {
            len: filters.len() as u16,
            filter: filters.as_ptr(),
        };
        let filter_ptr = &filter as *const sock_fprog;

        {
            let rc = libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, filter_ptr);
            if rc != 0 {
                return Err(*libc::__errno_location());
            }
        }
    }

    Ok(())
}

pub fn setup_sigsys_handler() -> Result<(), sys_util::Error> {
    return unsafe {
        sys_util::register_signal_handler(
            libc::SIGSYS,
            sys_util::SignalHandler::Siginfo(sigsys_handler),
            false,
        )
    };
}

extern "C" fn sigsys_handler(
    num: libc::c_int,
    info: *mut libc::siginfo_t,
    _unused: *mut libc::c_void,
) {
    if num != libc::SIGSYS {
        return;
    }
    let syscall = unsafe { *(info as *const i32).offset(SI_OFF_SYSCALL) as usize };
    METRICS.seccomp.num_faults.inc();
    METRICS.seccomp.bad_syscalls[syscall].inc();
}

/// Builds a "jump" BPF instruction.
#[allow(non_snake_case)]
fn BPF_JUMP(code: u16, k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter { code, jt, jf, k }
}

/// Builds a "statement" BPF instruction.
#[allow(non_snake_case)]
fn BPF_STMT(code: u16, k: u32) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

/// Builds a sequence of BPF instructions that validate the underlying architecture.
#[allow(non_snake_case)]
fn VALIDATE_ARCHITECTURE() -> Vec<sock_filter> {
    vec![
        BPF_STMT(BPF_LD + BPF_W + BPF_ABS, 4),
        BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_KILL),
    ]
}

/// Builds a sequence of BPF instructions that are followed by syscall examination.
#[allow(non_snake_case)]
fn EXAMINE_SYSCALL() -> Vec<sock_filter> {
    vec![BPF_STMT(BPF_LD + BPF_W + BPF_ABS, 0)]
}

/// Builds a sequence of BPF instructions that allow a syscall to go through.
#[allow(non_snake_case)]
fn ALLOW_SYSCALL(syscall_number: i64) -> Vec<sock_filter> {
    vec![
        BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, syscall_number as u32, 0, 1),
        BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_ALLOW),
    ]
}

/// Builds a sequence of BPF instructions that emit SIGSYS when a syscall is denied.
#[allow(non_snake_case)]
fn SIGNAL_PROCESS() -> Vec<sock_filter> {
    vec![BPF_STMT(BPF_RET + BPF_K, SECCOMP_RET_TRAP)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    /// Checks that rule gets translated correctly into BPF statements.
    #[test]
    fn test_rule_bpf_output() {
        // Builds rule.
        let rule = SeccompRule::new(
            vec![
                SeccompCondition::new(0, SeccompCmpOp::Eq, 1).unwrap(),
                SeccompCondition::new(2, SeccompCmpOp::MaskedEq(0b1010), 14).unwrap(),
            ],
            SeccompAction::Allow,
        );

        // Calculates architecture dependent argument value offsets.
        let (msb_offset, lsb_offset) = {
            #[cfg(target_endian = "big")]
            {
                (0, 4)
            }
            #[cfg(target_endian = "little")]
            {
                (4, 0)
            }
        };

        // Builds hardcoded BPF instructions.
        let instructions = vec![
            BPF_STMT(0x05, 1),
            BPF_STMT(0x05, 12),
            BPF_STMT(0x20, 32 + msb_offset),
            BPF_STMT(0x54, 0),
            BPF_JUMP(0x15, 0, 0, 8),
            BPF_STMT(0x20, 32 + lsb_offset),
            BPF_STMT(0x54, 0b1010),
            BPF_JUMP(0x15, 14 & 0b1010, 0, 5),
            BPF_STMT(0x20, 16 + msb_offset),
            BPF_JUMP(0x15, 0, 0, 3),
            BPF_STMT(0x20, 16 + lsb_offset),
            BPF_JUMP(0x15, 1, 0, 1),
            BPF_STMT(0x06, 0x7fff0000),
        ];

        // Compares translated rule with hardcoded BPF instructions.
        assert_eq!(rule.into_bpf(), instructions);
    }

    /// Checks that rule with too many conditions gets translated correctly into BPF statements
    /// using three helper jumps.
    #[test]
    fn test_rule_many_conditions_bpf_output() {
        // Builds rule.
        let mut conditions = Vec::with_capacity(43);
        for _ in 0..42 {
            conditions.push(SeccompCondition::new(0, SeccompCmpOp::MaskedEq(0), 0).unwrap());
        }
        conditions.push(SeccompCondition::new(0, SeccompCmpOp::Eq, 0).unwrap());
        let rule = SeccompRule::new(conditions, SeccompAction::Allow);

        // Calculates architecture dependent argument value offsets.
        let (msb_offset, lsb_offset) = {
            #[cfg(target_endian = "big")]
            {
                (0, 4)
            }
            #[cfg(target_endian = "little")]
            {
                (4, 0)
            }
        };

        // Builds hardcoded BPF instructions.
        let mut instructions = vec![
            BPF_STMT(0x05, 1),
            BPF_STMT(0x05, 6),
            BPF_STMT(0x20, 16 + msb_offset),
            BPF_JUMP(0x15, 0, 0, 3),
            BPF_STMT(0x20, 16 + lsb_offset),
            BPF_JUMP(0x15, 0, 0, 1),
            BPF_STMT(0x05, 2),
            BPF_STMT(0x05, 254),
            BPF_STMT(0x05, 254),
        ];
        let mut offset = 253;
        for _ in 0..42 {
            offset -= 6;
            instructions.append(&mut vec![
                BPF_STMT(0x20, 16 + msb_offset),
                BPF_STMT(0x54, 0),
                BPF_JUMP(0x15, 0, 0, offset + 3),
                BPF_STMT(0x20, 16 + lsb_offset),
                BPF_STMT(0x54, 0),
                BPF_JUMP(0x15, 0, 0, offset),
            ]);
        }
        instructions.push(BPF_STMT(0x06, 0x7fff0000));

        // Compares translated rule with hardcoded BPF instructions.
        assert_eq!(rule.into_bpf(), instructions);
    }

    #[test]
    fn test_context_bpf_output() {
        // Compares translated context with hardcoded BPF program.
        let context = SeccompFilterContext::new(
            vec![
                (
                    1,
                    (
                        1,
                        vec![
                            SeccompRule::new(
                                vec![
                                    SeccompCondition::new(2, SeccompCmpOp::Le, 14).unwrap(),
                                    SeccompCondition::new(2, SeccompCmpOp::Ne, 10).unwrap(),
                                ],
                                SeccompAction::Allow,
                            ),
                            SeccompRule::new(
                                vec![
                                    SeccompCondition::new(2, SeccompCmpOp::Gt, 20).unwrap(),
                                    SeccompCondition::new(2, SeccompCmpOp::Lt, 30).unwrap(),
                                ],
                                SeccompAction::Allow,
                            ),
                        ],
                    ),
                ),
                (
                    9,
                    (
                        0,
                        vec![SeccompRule::new(
                            vec![
                                SeccompCondition::new(1, SeccompCmpOp::MaskedEq(0b100), 36)
                                    .unwrap(),
                            ],
                            SeccompAction::Allow,
                        )],
                    ),
                ),
            ].into_iter()
            .collect(),
            SeccompAction::Trap,
        ).unwrap();
        let instructions = vec![
            BPF_STMT(0x20, 0),
            BPF_JUMP(0x15, 1, 0, 1),
            BPF_STMT(0x05, 1),
            BPF_STMT(0x05, 11),
            BPF_STMT(0x20, 36),
            BPF_JUMP(0x15, 0, 0, 2),
            BPF_STMT(0x20, 32),
            BPF_JUMP(0x15, 10, 6, 0),
            BPF_STMT(0x20, 36),
            BPF_JUMP(0x25, 0, 4, 0),
            BPF_JUMP(0x15, 0, 0, 2),
            BPF_STMT(0x20, 32),
            BPF_JUMP(0x25, 14, 1, 0),
            BPF_STMT(0x06, 0x7fff0000),
            BPF_STMT(0x05, 1),
            BPF_STMT(0x05, 12),
            BPF_STMT(0x20, 36),
            BPF_JUMP(0x25, 0, 9, 0),
            BPF_JUMP(0x15, 0, 0, 2),
            BPF_STMT(0x20, 32),
            BPF_JUMP(0x35, 30, 6, 0),
            BPF_STMT(0x20, 36),
            BPF_JUMP(0x25, 0, 3, 0),
            BPF_JUMP(0x15, 0, 0, 3),
            BPF_STMT(0x20, 32),
            BPF_JUMP(0x25, 20, 0, 1),
            BPF_STMT(0x06, 0x7fff0000),
            BPF_STMT(0x06, 0x00030000),
            BPF_JUMP(0x15, 9, 0, 1),
            BPF_STMT(0x05, 1),
            BPF_STMT(0x05, 8),
            BPF_STMT(0x20, 28),
            BPF_STMT(0x54, 0),
            BPF_JUMP(0x15, 0, 0, 4),
            BPF_STMT(0x20, 24),
            BPF_STMT(0x54, 0b100),
            BPF_JUMP(0x15, 36 & 0b100, 0, 1),
            BPF_STMT(0x06, 0x7fff0000),
            BPF_STMT(0x06, 0x00030000),
            BPF_STMT(0x06, 0x00030000),
        ];

        assert_eq!(context.into_bpf().unwrap(), instructions);
    }

    #[test]
    fn test_signal_handler() {
        assert!(setup_sigsys_handler().is_ok());

        // Syscalls that have to be allowed in order for the test to work.
        const REQUIRED_SYSCALLS: &[i64] = &[
            libc::SYS_exit,
            libc::SYS_futex,
            libc::SYS_munmap,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_set_tid_address,
            libc::SYS_sigaltstack,
        ];
        assert!(setup_seccomp(SeccompLevel::Basic(REQUIRED_SYSCALLS)).is_ok());

        // Calls the blacklisted SYS_getpid.
        let _pid = process::id();

        // The signal handler should let the program continue.
        assert!(true);

        // The reason this test doesn't check the failure metrics as well is that the signal handler
        // doesn't work right with kcov - possibly because the process is being pinned to 1 core.
    }

    #[test]
    fn test_bpf_expanding_functions() {
        // Compares the output of the BPF instruction generating functions to hardcoded
        // instructions.
        assert_eq!(
            BPF_STMT(BPF_LD + BPF_W + BPF_ABS, 16),
            sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 16,
            }
        );
        assert_eq!(
            BPF_JUMP(BPF_JMP + BPF_JEQ + BPF_K, 10, 2, 5),
            sock_filter {
                code: 0x15,
                jt: 2,
                jf: 5,
                k: 10,
            }
        );
    }

    #[test]
    fn test_bpf_functions() {
        {
            let ret = VALIDATE_ARCHITECTURE();
            let instructions = vec![
                sock_filter {
                    code: 32,
                    jt: 0,
                    jf: 0,
                    k: 4,
                },
                sock_filter {
                    code: 21,
                    jt: 1,
                    jf: 0,
                    k: 0xC000003E,
                },
                sock_filter {
                    code: 6,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
            ];
            assert_eq!(ret, instructions);
        }

        {
            let ret = EXAMINE_SYSCALL();
            let instructions = vec![sock_filter {
                code: 32,
                jt: 0,
                jf: 0,
                k: 0,
            }];
            assert_eq!(ret, instructions);
        }

        {
            let ret = ALLOW_SYSCALL(123);
            let instructions = vec![
                sock_filter {
                    code: 21,
                    jt: 0,
                    jf: 1,
                    k: 123,
                },
                sock_filter {
                    code: 6,
                    jt: 0,
                    jf: 0,
                    k: 0x7FFF0000,
                },
            ];
            assert_eq!(ret, instructions);
        }

        {
            let ret = SIGNAL_PROCESS();
            let instructions = vec![sock_filter {
                code: 6,
                jt: 0,
                jf: 0,
                k: 0x30000,
            }];
            assert_eq!(ret, instructions);
        }
    }
}
