/*
 * Generic intermediate code generation.
 *
 * Copyright (C) 2016-2017 Lluís Vilanova <vilanova@ac.upc.edu>
 *
 * This work is licensed under the terms of the GNU GPL, version 2 or later.
 * See the COPYING file in the top-level directory.
 */

#include "qemu/osdep.h"
#include "cpu.h"
#include "tcg/tcg.h"
#include "tcg/tcg-op.h"
#include "exec/exec-all.h"
#include "exec/gen-icount.h"
#include "exec/translator.h"

#include <uc_priv.h>

/* Pairs with tcg_clear_temp_count.
   To be called by #TranslatorOps.{translate_insn,tb_stop} if
   (1) the target is sufficiently clean to support reporting,
   (2) as and when all temporaries are known to be consumed.
   For most targets, (2) is at the end of translate_insn.  */
void translator_loop_temp_check(DisasContextBase *db)
{
#if 0
    if (tcg_check_temp_count()) {
        qemu_log("warning: TCG temporary leaks before "
                 TARGET_FMT_lx "\n", db->pc_next);
    }
#endif
}

void translator_loop(const TranslatorOps *ops, DisasContextBase *db,
                     CPUState *cpu, TranslationBlock *tb, int max_insns)
{
    int bp_insn = 0;
    struct uc_struct *uc = (struct uc_struct *)cpu->uc;
    TCGContext *tcg_ctx = uc->tcg_ctx;
    TCGOp *prev_op = NULL;
    bool block_hook = false;
    TCGOp *count_op = NULL;

    /* Initialize DisasContext */
    db->tb = tb;
    db->pc_first = tb->pc;
    db->pc_next = db->pc_first;
    db->is_jmp = DISAS_NEXT;
    db->num_insns = 0;
    db->max_insns = max_insns;
    db->singlestep_enabled = cpu->singlestep_enabled;

    ops->init_disas_context(db, cpu);
    tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

    /* Reset the temp count so that we can identify leaks */
    tcg_clear_temp_count();

    /* Unicorn: early check to see if the address of this block is
     * the "run until" address. */
    if (uc_addr_is_exit(uc, tb->pc)) {
        // This should catch that instruction is at the end
        // and generate appropriate halting code.
        gen_tb_start(tcg_ctx, db->tb);
        ops->tb_start(db, cpu);
        db->num_insns++;
        ops->insn_start(db, cpu);
        ops->translate_insn(db, cpu);
        goto _end_loop;
    }

    /* Unicorn: trace this block on request
     * Only hook this block if it is not broken from previous translation due to
     * full translation cache
     */
    if (uc->fc_insns) {
        /* Drydock: count the block inline instead of calling a hook (0004). */
        TCGv_ptr pins = tcg_const_ptr(tcg_ctx, uc->fc_insns);
        TCGv_ptr plim = tcg_const_ptr(tcg_ctx, uc->fc_limit);
        TCGv_i64 cur = tcg_temp_new_i64(tcg_ctx);
        TCGv_i64 lim = tcg_temp_new_i64(tcg_ctx);
        TCGLabel *stop = gen_new_label(tcg_ctx);
        TCGLabel *done = gen_new_label(tcg_ctx);

        ops->pc_sync(db, cpu);
        tcg_gen_ld_i64(tcg_ctx, cur, pins, 0);
        tcg_gen_ld_i64(tcg_ctx, lim, plim, 0);
        tcg_gen_brcond_i64(tcg_ctx, TCG_COND_GEU, cur, lim, stop);

        /* Temps do not live across the branch: reload and count. */
        TCGv_ptr pins2 = tcg_const_ptr(tcg_ctx, uc->fc_insns);
        TCGv_ptr pblk = tcg_const_ptr(tcg_ctx, uc->fc_blocks);
        TCGv_i64 cnt = tcg_temp_new_i64(tcg_ctx);
        TCGv_i64 blk = tcg_temp_new_i64(tcg_ctx);
        TCGv_i64 n = tcg_const_i64(tcg_ctx, 0xf8f8f8f8);
        count_op = tcg_last_op(tcg_ctx);
        tcg_gen_ld_i64(tcg_ctx, cnt, pins2, 0);
        tcg_gen_add_i64(tcg_ctx, cnt, cnt, n);
        tcg_gen_st_i64(tcg_ctx, cnt, pins2, 0);
        tcg_gen_ld_i64(tcg_ctx, blk, pblk, 0);
        tcg_gen_addi_i64(tcg_ctx, blk, blk, 1);
        tcg_gen_st_i64(tcg_ctx, blk, pblk, 0);
        tcg_gen_br(tcg_ctx, done);

        gen_set_label(tcg_ctx, stop);
        TCGv_ptr puc = tcg_const_ptr(tcg_ctx, uc);
        gen_helper_icount_stop(tcg_ctx, puc);
        gen_set_label(tcg_ctx, done);
        tcg_temp_free_ptr(tcg_ctx, puc);
        tcg_temp_free_i64(tcg_ctx, n);
        tcg_temp_free_i64(tcg_ctx, blk);
        tcg_temp_free_i64(tcg_ctx, cnt);
        tcg_temp_free_ptr(tcg_ctx, pblk);
        tcg_temp_free_ptr(tcg_ctx, pins2);
        tcg_temp_free_i64(tcg_ctx, lim);
        tcg_temp_free_i64(tcg_ctx, cur);
        tcg_temp_free_ptr(tcg_ctx, plim);
        tcg_temp_free_ptr(tcg_ctx, pins);
    } else if (HOOK_EXISTS_BOUNDED(uc, UC_HOOK_BLOCK, tb->pc)) {
        ops->pc_sync(db, cpu);
        prev_op = tcg_last_op(tcg_ctx);
        block_hook = true;
        gen_uc_tracecode(tcg_ctx, 0xf8f8f8f8, UC_HOOK_BLOCK_IDX, uc, db->pc_first);
    }

    // tcg_dump_ops(tcg_ctx, false, "translator loop");

    /* Start translating.  */
    gen_tb_start(tcg_ctx, db->tb);
    // tcg_dump_ops(tcg_ctx, false, "tb start");

    ops->tb_start(db, cpu);
    // tcg_dump_ops(tcg_ctx, false, "tb start 2");

    tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

    while (true) {
        db->num_insns++;

        ops->insn_start(db, cpu);
        tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

        /* Pass breakpoint hits to target for further processing */
        if (!db->singlestep_enabled
            && unlikely(!QTAILQ_EMPTY(&cpu->breakpoints))) {
            CPUBreakpoint *bp;
            QTAILQ_FOREACH(bp, &cpu->breakpoints, entry) {
                if (bp->pc == db->pc_next) {
                    if (ops->breakpoint_check(db, cpu, bp)) {
                        bp_insn = 1;
                        break;
                    }
                }
            }
            /* The breakpoint_check hook may use DISAS_TOO_MANY to indicate
               that only one more instruction is to be executed.  Otherwise
               it should use DISAS_NORETURN when generating an exception,
               but may use a DISAS_TARGET_* value for Something Else.  */
            if (db->is_jmp > DISAS_TOO_MANY) {
                break;
            }
        }

        /* Disassemble one instruction.  The translate_insn hook should
           update db->pc_next and db->is_jmp to indicate what should be
           done next -- either exiting this loop or locate the start of
           the next instruction.  */
        ops->translate_insn(db, cpu);
        // tcg_dump_ops(tcg_ctx, false, "insn translate");

        /* Stop translation if translate_insn so indicated.  */
        if (db->is_jmp != DISAS_NEXT) {
            break;
        }

        /* Stop translation if the output buffer is full,
           or we have executed all of the allowed instructions.  */
        if (tcg_op_buf_full(tcg_ctx) || db->num_insns >= db->max_insns) {
            db->is_jmp = DISAS_TOO_MANY;
            break;
        }
    }

_end_loop:
    /* Emit code to exit the TB, as indicated by db->is_jmp.  */
    ops->tb_stop(db, cpu);
    gen_tb_end(tcg_ctx, db->tb, db->num_insns - bp_insn);
    // tcg_dump_ops(tcg_ctx, false, "tb end");

    /* The disas_log hook may use these values rather than recompute.  */
    db->tb->size = db->pc_next - db->pc_first;
    db->tb->icount = db->num_insns;

    hooked_regions_check(uc, db->tb->pc, db->tb->size);

    if (count_op) {
        /* Drydock (0004): patch in the block's instruction count and record
         * the pages that hold translated code. */
        uint32_t first = db->tb->pc >> 12;
        uint32_t last = (db->tb->pc + (db->tb->size ? db->tb->size : 1) - 1) >> 12;
        uint32_t pg;
        count_op->args[1] = db->tb->icount;
        for (pg = first; pg <= last; pg++) {
            uc->fc_code_pages[(pg & 0xfffff) >> 6] |= UINT64_C(1) << (pg & 63);
        }
    }

    if (block_hook) {
        TCGOp *tcg_op;

        // Unicorn: patch the callback to have the proper block size.
        if (prev_op) {
            // As explained further up in the function where prev_op is
            // assigned, we move forward in the tail queue, so we're modifying the
            // move instruction generated by gen_uc_tracecode() that contains
            // the instruction size to assign the proper size (replacing 0xF1F1F1F1).
            tcg_op = QTAILQ_NEXT(prev_op, link);
        } else {
            // this basic block is the first emulated code ever,
            // so the basic block operand is the first operand
            tcg_op = QTAILQ_FIRST(&tcg_ctx->ops);
        }

        /* Drydock: the upper 16 bits carry the block's instruction count, so the
         * hook does not have to decode the block to count it. Blocks are smaller
         * than 64 KiB and hold at most TCG_MAX_INSNS instructions. */
        tcg_op->args[1] = db->tb->size | ((uint32_t)db->tb->icount << 16);
    }
}
