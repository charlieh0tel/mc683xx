use super::AddressTable;
use super::Adjustment;
use super::Cost;
use super::Decrement;
use super::branch_taken;
use super::decrement_and_branch;
use super::decrement_and_branch_looping;
use super::divide_long;
use super::exception;
use super::extension;
use super::full_extension;
use super::lpstop;
use super::movem;
use super::moves;
use super::multiply_long;
use super::return_from_exception;
use super::rows;
use super::shift_count;
use super::table;
use super::table_lookup;
use super::trap_taken;
use crate::cycles::Cycles;
use crate::instructions::tests::machine;
use crate::instructions::tests::step;
use crate::size::Size;

/// The table's cost of `opcode`.
fn cost(opcode: u16) -> Cost {
    table()[usize::from(opcode)]
}

/// A cost.
const fn costs(head: u8, tail: u8, cycles: u16) -> Cost {
    Cost { head, tail, cycles }
}

/// What a run of `opcodes` costs by CPU32RM 8.1.6, starting with no
/// tail, as the run loop charges it.
fn sequence(opcodes: &[u16]) -> u64 {
    opcodes
        .iter()
        .fold((0, 0), |(total, tail), &opcode| {
            let cost = cost(opcode);
            (
                total + u64::from(cost.cycles - u16::from(tail.min(cost.head))),
                cost.tail,
            )
        })
        .0
}

#[test]
fn add_long_indexed_is_the_manuals_worked_example() {
    // ADD.L (12, A3, D7.W*4), D2: 12 clocks, head 4 (CPU32RM 8.3).
    assert_eq!(cost(0xd4b3), costs(4, 0, 12));
}

#[test]
fn a_long_write_overlaps_lsl_as_the_manual_says() {
    // MOVE.L D0, (A0) then LSL.L #7, D2: 8 clocks, not 12 (CPU32RM 8.3).
    assert_eq!(cost(0x2080), costs(0, 4, 6));
    assert_eq!(cost(0xef8a), costs(4, 0, 6));
    assert_eq!(sequence(&[0x2080, 0xef8a]), 8);
}

#[test]
fn example_1_saves_a_clock_between_instructions() {
    // MOVE.W A1, (A0)+; ADDQ.W #1, (A0); CLR.W $30(A1): 18 clocks
    // (CPU32RM 8.2.1, Figure 8-4).
    assert_eq!(sequence(&[0x30c9, 0x5250, 0x4269]), 18);
}

#[test]
fn example_2_branch_not_taken_and_taken() {
    // MOVEQ #7, D1; CMP.L D1, D0; BLE.B; MOVE.L D1, (A0): 14 clocks
    // not taken (CPU32RM 8.2.2, Figure 8-6).
    assert_eq!(sequence(&[0x7207, 0xb081, 0x6f04, 0x2081]), 14);
    let taken = sequence(&[0x7207, 0xb081, 0x6f04]) + branch_taken(Size::Byte).0;
    assert_eq!(taken, 2 + 2 + 8);
}

#[test]
fn the_run_loop_charges_the_table_and_the_overlap() {
    // NOP, MOVEQ #1, D0
    let mut cpu = machine(&[0x4e71, 0x7001]);
    assert_eq!(step(&mut cpu, 1).cycles, Cycles(2));
    assert_eq!(step(&mut cpu, 1).cycles, Cycles(2));
    // MOVE.L D0, (A0) (tail 4); MOVE.L D1, -(A1) (head 2, 8 cycles).
    let mut cpu = machine(&[0x2080, 0x2301]);
    cpu.registers.set_address(0, 0x2000);
    cpu.registers.set_address(1, 0x3000);
    assert_eq!(step(&mut cpu, 1).cycles, Cycles(6));
    assert_eq!(step(&mut cpu, 1).cycles, Cycles(8 - 2));
}

#[test]
fn move_memory_to_indexed_memory_runs_its_table_cost() {
    // MOVE.L (A0)+, (8, A1, D2.W*4): CEA (An)+ 2, FEA index long 10,
    // MOVE <CEA>, <FEA> long 8, less min(4, 2).
    assert_eq!(cost(0x2398), costs(1, 4, 2 + 10 - 2 + 8));
    let mut cpu = machine(&[0x2398, 0x2408]);
    cpu.registers.set_address(0, 0x2000);
    cpu.registers.set_address(1, 0x3000);
    assert_eq!(step(&mut cpu, 1).cycles, Cycles(18));
}

#[test]
fn fetch_effective_address_rows() {
    // TST <FEA>: the fetch row plus TST's 2, head 0 (CPU32RM 8.3.1).
    assert_eq!(cost(0x4a40), costs(0, 0, 2)); // TST.W D0
    assert_eq!(cost(0x4a50), costs(1, 0, 5)); // (A0)
    assert_eq!(cost(0x4a60), costs(2, 0, 6)); // -(A0)
    assert_eq!(cost(0x4a68), costs(1, 0, 7)); // (d16, A0)
    assert_eq!(cost(0x4a7a), costs(1, 0, 7)); // (d16, PC)
    assert_eq!(cost(0x4a70), costs(4, 0, 10)); // (d8, A0, Xn)
    assert_eq!(cost(0x4a78), costs(1, 0, 7)); // (xxx).W
    assert_eq!(cost(0x4a79), costs(1, 0, 9)); // (xxx).L
    assert_eq!(cost(0x4a7c), costs(1, 0, 5)); // #.W
    assert_eq!(cost(0x4abc), costs(1, 0, 7)); // TST.L #
    assert_eq!(cost(0x4a90), costs(1, 0, 7)); // TST.L (A0): X
    assert_eq!(cost(0x4a08), cost(0x4afc)); // TST.B A0: illegal
}

#[test]
fn calculate_effective_address_rows() {
    // LEA <CEA>, A1: the calculate row plus LEA's 2 (CPU32RM 8.3.2).
    assert_eq!(cost(0x43d0), costs(1, 0, 4)); // (A0)
    assert_eq!(cost(0x43e8), costs(1, 0, 5)); // (d16, A0)
    assert_eq!(cost(0x43f0), costs(4, 0, 8)); // (d8, A0, Xn)
    assert_eq!(cost(0x43f9), costs(1, 0, 7)); // (xxx).L
    // JSR (d16, PC): 3 - min(1, 3) + 13.
    assert_eq!(cost(0x4eba), costs(1, 0, 15));
    assert_eq!(cost(0x4ed0), costs(1, 0, 8)); // JMP (A0)
    assert_eq!(cost(0x4850), costs(1, 0, 10)); // PEA (A0)
    assert_eq!(cost(0x43d8), cost(0x4afc)); // LEA (A0)+: illegal
}

#[test]
fn move_rows_first_match_wins() {
    // CPU32RM 8.3.3.
    assert_eq!(cost(0x3200), costs(0, 0, 2)); // MOVE.W D0, D1
    assert_eq!(cost(0x2248), costs(0, 0, 2)); // MOVEA.L A0, A1
    assert_eq!(cost(0x3210), costs(1, 0, 5)); // MOVE.W (A0), D1
    assert_eq!(cost(0x3280), costs(0, 2, 4)); // MOVE.W D0, (A1)
    assert_eq!(cost(0x32c0), costs(1, 1, 5)); // MOVE.W D0, (A1)+
    assert_eq!(cost(0x3340), costs(1, 3, 7)); // MOVE.W D0, (d16, A1)
    assert_eq!(cost(0x3290), costs(1, 2, 8)); // MOVE.W (A0), (A1)
    assert_eq!(cost(0x337c), costs(1, 2, 10)); // MOVE.W #, (d16, A1)
    // MOVE.L (d16, A0), (d16, A1): 3 - 1 + 7 - 2 + 8.
    assert_eq!(cost(0x2368), costs(1, 4, 15));
    assert_eq!(cost(0x1008), cost(0x4afc)); // MOVE.B A0, D0: illegal
    assert_eq!(cost(0x1048), cost(0x4afc)); // MOVEA.B: illegal
}

#[test]
fn immediate_and_bit_rows() {
    // CPU32RM 8.3.6, 8.3.10, 8.3.12, 8.3.4.
    assert_eq!(cost(0x0680), costs(1, 0, 7)); // ADDI.L #, D0
    assert_eq!(cost(0x0650), costs(1, 3, 10)); // ADDI.W #, (A0)
    assert_eq!(cost(0x0c3a), costs(1, 3, 12)); // CMPI.B #, (d16, PC)
    assert_eq!(cost(0x0c3c), cost(0x4afc)); // CMPI.B #, #: illegal
    assert_eq!(cost(0x007c), costs(0, 0, 12)); // ORI to SR
    assert_eq!(cost(0x023c), costs(2, 0, 6)); // ANDI to CCR
    assert_eq!(cost(0x0800), costs(1, 0, 6)); // BTST #, D0
    assert_eq!(cost(0x03d0), costs(1, 2, 10)); // BSET D1, (A0)
    assert_eq!(cost(0x0148), costs(1, 2, 19)); // MOVEP.L (d16, A0), D0
    assert_eq!(cost(0x5250), costs(1, 3, 8)); // ADDQ.W #1, (A0)
    assert_eq!(cost(0x5248), costs(0, 0, 2)); // ADDQ.W #1, A0
    assert_eq!(cost(0x5208), cost(0x4afc)); // ADDQ.B #1, A0: illegal
}

#[test]
fn single_operand_and_extended_rows() {
    // CPU32RM 8.3.7, 8.3.8.
    assert_eq!(cost(0x4290), costs(1, 4, 8)); // CLR.L (A0): 2 + 6
    assert_eq!(cost(0x4480), costs(0, 0, 2)); // NEG.L D0
    assert_eq!(cost(0x4ad0), costs(1, 0, 12)); // TAS (A0)
    assert_eq!(cost(0x57c0), costs(2, 0, 4)); // SEQ D0
    assert_eq!(cost(0xc308), costs(2, 2, 12)); // ABCD -(A0), -(A1)
    assert_eq!(cost(0xd388), costs(2, 4, 16)); // ADDX.L -(A0), -(A1)
    assert_eq!(cost(0xb388), costs(1, 2, 12)); // CMPM.L (A0)+, (A1)+
    assert_eq!(cost(0xc141), costs(2, 0, 4)); // EXG D0, D1
    assert_eq!(cost(0x4840), costs(4, 0, 6)); // SWAP D0
    assert_eq!(cost(0x49c0), costs(0, 0, 2)); // EXTB.L D0
}

#[test]
fn shift_counts_match_the_manuals_table() {
    // CPU32RM 8.3.9: clocks, then the counts that take them.
    let listed: &[(u16, &[u32])] = &[
        (6, &[0, 1, 2, 3, 4, 5, 6, 8, 9, 12]),
        (8, &[7, 10, 11, 13, 14, 16, 17, 20]),
        (10, &[15, 18, 19, 21, 22, 24, 25, 28]),
        (12, &[23, 26, 27, 29, 30, 32, 33, 36]),
        (14, &[31, 34, 35, 37, 38, 40, 41, 44]),
        (16, &[39, 42, 43, 45, 46, 48, 49, 52]),
        (18, &[47, 50, 51, 53, 54, 56, 57, 60]),
        (20, &[55, 58, 59, 61, 62]),
        (22, &[63]),
    ];
    for &(clocks, counts) in listed {
        for &count in counts {
            assert_eq!(rows::shift::register_clocks(count), clocks, "count {count}");
        }
    }
}

#[test]
fn shift_rows() {
    // CPU32RM 8.3.9.
    assert_eq!(cost(0xe3a8), costs(4, 0, 6)); // LSL.L D1, D0
    assert_eq!(shift_count(0xe3a8, 63), Cycles(22 - 6));
    // ROXL.L D1, D0 by 8: max(3 + 8 + 1, 6) (note 2).
    assert_eq!(shift_count(0xe3b0, 8), Cycles(12 - 6));
    assert_eq!(cost(0xe150), costs(8, 0, 10)); // ROXL.W #8, D0 (note 3)
    assert_eq!(cost(0xe210), costs(4, 0, 6)); // ROXR.B #1, D0
    assert_eq!(cost(0xe3d0), costs(1, 2, 9)); // LSL.W (A0)
    assert_eq!(cost(0xe8c0), cost(0x4afc)); // BFTST: illegal
}

#[test]
fn branch_rows() {
    // CPU32RM 8.3.11, 8.3.12.
    assert_eq!(cost(0x6002), costs(2, 0, 8)); // BRA.B
    assert_eq!(cost(0x6700), costs(0, 0, 4)); // BEQ.W, not taken
    assert_eq!(cost(0x67ff), costs(0, 0, 6)); // BEQ.L, not taken
    assert_eq!(cost(0x61ff), costs(1, 0, 13)); // BSR.L
    assert_eq!(branch_taken(Size::Byte), Cycles(4));
    assert_eq!(branch_taken(Size::Word), Cycles(4));
    assert_eq!(branch_taken(Size::Long), Cycles(2));
    assert_eq!(cost(0x51c8), costs(1, 1, 4)); // DBF D0
    assert_eq!(decrement_and_branch(Decrement::ConditionTrue), Cycles::ZERO);
    assert_eq!(decrement_and_branch(Decrement::Expired), Cycles(2));
    assert_eq!(decrement_and_branch(Decrement::Taken), Cycles(6));
}

#[test]
fn loop_mode_dbcc_rows() {
    // CPU32RM 8.3.11, "in loop mode": (T) 4, 0, 6; (F, -1) 6, 0, 8;
    // (F, not -1) 6, 0, 10; against (T) 1, 1, 4, each overlapping the
    // last tail by its own head.
    let more = |clocks| Adjustment::More(Cycles(clocks));
    for (last_tail, true_, expired, taken) in [
        (0, more(2), more(4), more(6)),
        (2, more(1), more(3), more(5)),
        // 6 - 4 against 4 - 1, and so on.
        (4, Adjustment::Fewer(Cycles(1)), more(1), more(3)),
    ] {
        assert_eq!(
            decrement_and_branch_looping(Decrement::ConditionTrue, last_tail),
            true_
        );
        assert_eq!(
            decrement_and_branch_looping(Decrement::Expired, last_tail),
            expired
        );
        assert_eq!(
            decrement_and_branch_looping(Decrement::Taken, last_tail),
            taken
        );
    }
}

#[test]
fn special_move_rows() {
    // CPU32RM 8.3.4.
    // MOVEM.L D0-D7, -(A7): 2 + 10 plus 8 * 4.
    assert_eq!(cost(0x48e7), costs(2, 2, 12));
    assert_eq!(movem(8), Cycles(32));
    assert_eq!(cost(0x4cdf), costs(1, 0, 14)); // MOVEM.L (A7)+, RL
    assert_eq!(cost(0x48d8), cost(0x4afc)); // MOVEM to (A0)+: illegal
    assert_eq!(cost(0x4e7a), costs(10, 0, 14)); // MOVEC Cr, Rn
    assert_eq!(cost(0x4e7b), costs(12, 0, 16)); // MOVEC Rn, Cr
    assert_eq!(cost(0x46c0), costs(4, 0, 10)); // MOVE D0, SR
    assert_eq!(cost(0x40d0), costs(1, 2, 6)); // MOVE SR, (A0)
    // MOVES.W (A0), Rn: save 3, CEA 2, op 11, less min(1, 1).
    assert_eq!(cost(0x0e50), costs(1, 1, 15));
    assert_eq!(moves(0), Cycles::ZERO);
    assert_eq!(moves(extension::REGISTER_TO_MEMORY), Cycles(1));
}

#[test]
fn multiply_divide_and_table_rows() {
    // CPU32RM 8.3.5.
    assert_eq!(cost(0xc0c1), costs(0, 0, 26)); // MULU.W D1, D0
    assert_eq!(cost(0x81d0), costs(1, 0, 45)); // DIVS.W (A0), D0
    // MULU.L (A0), D0: save 3, FEA long 5, op 46: 3 - 1 + 5 - 2 + 46.
    assert_eq!(cost(0x4c10), costs(1, 0, 51));
    assert_eq!(multiply_long(0), Cycles(6));
    assert_eq!(multiply_long(extension::QUAD), Cycles::ZERO);
    assert_eq!(cost(0x4c41), costs(2, 0, 46)); // DIVS.L D1, D0: no save
    assert_eq!(divide_long(extension::SIGNED), Cycles(16));
    assert_eq!(divide_long(0), Cycles::ZERO);
    // CMP2.W (A0), Rn: save 3, FEA 3, op 18: 3 - 1 + 3 - 1 + 18.
    assert_eq!(cost(0x02d0), costs(1, 0, 22));
    // TBLU.W (A0), D0: save 3, CEA 2, op 35: 3 - 1 + 2 + 35.
    assert_eq!(cost(0xf810), costs(1, 0, 39));
    assert_eq!(cost(0xf801), costs(26, 0, 30)); // TBLU D1:D?, D?
    let signed_unrounded_long = extension::SIGNED | extension::UNROUNDED | 0b10 << 6;
    assert_eq!(
        table_lookup(0xf810, signed_unrounded_long),
        Cycles(39 + 4 - 35)
    );
    assert_eq!(table_lookup(0xf801, extension::UNROUNDED), Cycles(40 - 30));
    assert_eq!(table_lookup(0xf801, extension::SIGNED), Cycles::ZERO);
}

#[test]
fn control_and_exception_rows() {
    // CPU32RM 8.3.12, 8.3.13, 8.3.14.
    assert_eq!(cost(0x4e71), costs(0, 0, 2)); // NOP
    assert_eq!(cost(0x4e75), costs(1, 0, 12)); // RTS
    assert_eq!(cost(0x4e56), costs(2, 0, 10)); // LINK.W A6
    assert_eq!(cost(0x4e5e), costs(1, 0, 9)); // UNLK A6
    assert_eq!(cost(0x4e72), costs(2, 0, 12)); // STOP
    assert_eq!(cost(0x4e70), costs(0, 0, 518)); // RESET
    assert_eq!(cost(0x4e4f), costs(4, 0, 29)); // TRAP #15
    assert_eq!(cost(0x4e73), costs(1, 0, 24)); // RTE
    assert_eq!(cost(0x484f), costs(0, 0, 14)); // BKPT #7
    assert_eq!(cost(0x4180), costs(2, 0, 8)); // CHK.W D0, D0
    assert_eq!(return_from_exception(0), Cycles::ZERO);
    assert_eq!(return_from_exception(exception::SIX_WORD_FORMAT), Cycles(2));
    assert_eq!(
        return_from_exception(exception::BUS_ERROR_FORMAT),
        Cycles(26)
    );
    for illegal in [0x4afc, 0x4afa, 0x4e7c, 0x7100, 0x8140] {
        assert_eq!(cost(illegal), exception::ILLEGAL, "{illegal:#06x}");
    }
    assert_eq!(cost(0xa123), exception::LINE_A);
    assert_eq!(cost(0xf000), exception::LINE_F);
    assert_eq!(cost(0xf8c0), exception::LINE_F);
}

#[test]
fn traps_add_their_exception_rows() {
    // CPU32RM 8.3.12, 8.3.13.
    assert_eq!(trap_taken(0x4180), Cycles(42 - 8)); // CHK.W
    assert_eq!(trap_taken(0x41b9), Cycles(42 - 8)); // CHK.W (xxx).L
    assert_eq!(trap_taken(0x00d0), Cycles(52 - 18)); // CHK2.B (A0)
    assert_eq!(trap_taken(0x4e76), Cycles(38 - 4)); // TRAPV
    assert_eq!(trap_taken(0x51fb), Cycles(38 - 6)); // TRAPF.L
    assert_eq!(trap_taken(0x50fc), Cycles(38 - 4)); // TRAPT
    assert_eq!(trap_taken(0x4e71), Cycles::ZERO);
}

#[test]
fn exception_costs() {
    // CPU32RM 8.3.13, 8.3.14: the cycles, negative tails as zero.
    let expected = [
        (exception::ILLEGAL, 25),
        (exception::LINE_A, 25),
        (exception::LINE_F, 25),
        (exception::PRIVILEGE_VIOLATION, 25),
        (exception::FORMAT_ERROR, 25),
        (exception::TRACE, 36),
        (exception::ZERO_DIVIDE, 36),
        (exception::INTERRUPT, 30),
        (exception::BREAKPOINT, 10),
        (exception::BREAKPOINT_BUS_ERROR, 42),
        (exception::BKPT_BUS_ERROR, 35),
        (exception::BUS_ERROR, 58),
        (exception::BUS_ERROR_IN_EXCEPTION, 48),
        (exception::RTE_BUS_ERROR_FOUR_WORD, 66),
        (exception::RTE_BUS_ERROR_SIX_WORD, 70),
    ];
    for (cost, cycles) in expected {
        assert_eq!(cost.cycles, cycles);
        assert_eq!(cost.tail, 0);
    }
    assert_eq!(exception::line_f_second_word(0xf800), costs(1, 0, 31));
    // Save 3, CEA (A0) 2, op 29: 3 - 1 + 2 + 29.
    assert_eq!(exception::line_f_second_word(0xf810), costs(1, 0, 33));
}

#[test]
fn full_extension_words_against_the_brief_format() {
    // CPU32RM 8.3.1 against (d8, An, Xn)'s 8: BS, IS, BD SIZE.  BD SIZE
    // 00 is reserved and charged as 11.
    let fewer = |clocks| Adjustment::Fewer(Cycles(clocks));
    let more = |clocks| Adjustment::More(Cycles(clocks));
    let cases = [
        (0x01d0, fewer(2)), // (0)
        (0x01e0, fewer(1)), // (d16)
        (0x01f0, more(1)),  // (d32)
        (0x0150, fewer(3)), // (An)
        (0x0190, more(0)),  // (Xm)
        (0x0110, more(0)),  // (An, Xm)
        (0x0160, fewer(1)), // (d16, An)
        (0x0170, more(1)),  // (d32, An)
        (0x0120, more(0)),  // (d16, An, Xm)
        (0x0130, more(1)),  // (d32, An, Xm)
        (0x01a0, more(0)),  // (d16, Xm)
        (0x0100, more(1)),  // BD SIZE 00
    ];
    for (extension, adjustment) in cases {
        assert_eq!(
            full_extension(extension, AddressTable::Fetch),
            adjustment,
            "{extension:#06x}"
        );
    }
    // The calculate table (8.3.2) against its (d8, An, Xn)'s 6 differs
    // by the same, but for (An): 4 against 6.
    for extension in (0..1 << 8).map(|bits: u16| bits & 0xf0 | 0x100) {
        let expected = match extension {
            0x0150 => fewer(2),
            _ => full_extension(extension, AddressTable::Fetch),
        };
        assert_eq!(
            full_extension(extension, AddressTable::Calculate),
            expected,
            "{extension:#06x}"
        );
    }
}

#[test]
fn the_run_loop_charges_full_extension_words() {
    // TST.W (A0, D1) brief: FEA index 8, TST 2.  Full, (An): 5 + 2.
    // Full, (d32, A0, D1): 9 + 2.
    let brief = cost(0x4a70).cycles;
    assert_eq!(brief, 8 + 2);
    for (words, cycles) in [
        (&[0x4a70, 0x1000][..], brief),
        (&[0x4a70, 0x1150], brief - 3),
        (&[0x4a70, 0x1130, 0, 0x10], brief + 1),
    ] {
        let mut cpu = machine(words);
        cpu.registers.set_address(0, 0x2000);
        assert_eq!(step(&mut cpu, 1).cycles, Cycles(u64::from(cycles)));
    }
}

#[test]
fn calculated_addresses_take_the_calculate_tables_full_format() {
    // Full format (An), extension $1150, against the brief: CPU32RM
    // 8.3.1, 5 against 8; 8.3.2, 4 against 6.
    //   LEA (A0, D1), A1: CEA (4, 0, 6) then LEA (0, 0, 2), 8.
    //   MOVE.W D0, (A1, D1): MOVE Rn, <CEA>, the destination
    //   calculated (8.3.3).
    //   MOVE.W (A0, D1), (d16, A1): MOVE <CEA>, <FEA>, the source
    //   calculated (8.3.3, note).
    //   MOVE.W (A0, D1), D0: MOVE <FEA>, Rn, fetched.
    assert_eq!(cost(0x43f0).cycles, 8);
    let cases: [(&[u16], &[u16], u16); 4] = [
        (&[0x43f0, 0x1000], &[0x43f0, 0x1150], 2),
        (&[0x3380, 0x1000], &[0x3380, 0x1150], 2),
        (&[0x3370, 0x1000, 0x10], &[0x3370, 0x1150, 0x10], 2),
        (&[0x3030, 0x1000], &[0x3030, 0x1150], 3),
    ];
    for (brief, full, fewer) in cases {
        let run = |words: &[u16]| {
            let mut cpu = machine(words);
            cpu.registers.set_address(0, 0x2000);
            cpu.registers.set_address(1, 0x2000);
            step(&mut cpu, 1).cycles
        };
        assert_eq!(u64::from(cost(brief[0]).cycles), run(brief).0, "{brief:x?}");
        assert_eq!(
            run(full),
            Cycles(run(brief).0 - u64::from(fewer)),
            "{full:x?}"
        );
    }
}

#[test]
fn lpstop_takes_back_tbls_cost() {
    // TBL Dym:Dyn (26, 0, 30) against LPSTOP (3, -2, 25), CPU32RM 8.3.5,
    // 8.3.13: 5 clocks fewer, less the overlap past LPSTOP's head of 3.
    assert_eq!(cost(0xf800), costs(26, 0, 30));
    assert_eq!(lpstop(0), Adjustment::Fewer(Cycles(5)));
    assert_eq!(lpstop(2), Adjustment::Fewer(Cycles(5)));
    assert_eq!(lpstop(4), Adjustment::Fewer(Cycles(5 - 1)));
    assert_eq!(lpstop(10), Adjustment::More(Cycles(10 - 3 - 5)));
}

#[test]
fn every_opcode_costs_at_least_one_fetch() {
    assert!(table().iter().all(|cost| cost.cycles >= 2));
}
