//! End-to-end tests for the escrow, driven through LiteSVM.
//!
//! Mints and pre-funded token accounts are written straight into the SVM instead
//! of being created through the token program. This suite is about the escrow,
//! not about SPL, and skipping those CPIs keeps each test to a handful of lines.
//!
//! Accounts the escrow is supposed to create itself (the vault, and any missing
//! ATA) are deliberately left out, so `init` / `init_if_needed` stay covered.
//!
//! mint_a and mint_b use different decimals on purpose: a transfer that asserts
//! the wrong mint's decimals fails, so the swap test catches that class of bug.

use {
    anchor_lang::{
        prelude::Pubkey,
        solana_program::{instruction::Instruction, system_program},
        AccountDeserialize, InstructionData, ToAccountMetas,
    },
    escrow_new::{state::EscrowState, ESCROW_SEED},
    litesvm::{types::TransactionResult, LiteSVM},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

const PROGRAM_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/escrow_new.so"
));

const TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;
const ATA_PROGRAM: Pubkey = anchor_spl::associated_token::ID;

const MINT_A_DECIMALS: u8 = 6;
const MINT_B_DECIMALS: u8 = 9;

const SEED: u64 = 42;
const DEPOSIT: u64 = 100_000_000; // 100 of mint_a
const RECEIVE: u64 = 250_000_000_000; // 250 of mint_b

// ---------------------------------------------------------------- fixtures

/// One escrow scenario: a maker holding mint_a, a taker holding mint_b, and
/// every address both sides need. Nothing is on chain yet beyond the balances.
struct World {
    svm: LiteSVM,
    maker: Keypair,
    taker: Keypair,
    mint_a: Pubkey,
    mint_b: Pubkey,
    maker_ata_a: Pubkey,
    taker_ata_b: Pubkey,
    escrow: Pubkey,
    vault: Pubkey,
}

impl World {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(escrow_new::id(), PROGRAM_BYTES).unwrap();

        let maker = Keypair::new();
        let taker = Keypair::new();
        svm.airdrop(&maker.pubkey(), 10_000_000_000).unwrap();
        svm.airdrop(&taker.pubkey(), 10_000_000_000).unwrap();

        let mint_a = write_mint(&mut svm, MINT_A_DECIMALS);
        let mint_b = write_mint(&mut svm, MINT_B_DECIMALS);

        let maker_ata_a = write_token_account(&mut svm, &mint_a, &maker.pubkey(), DEPOSIT);
        let taker_ata_b = write_token_account(&mut svm, &mint_b, &taker.pubkey(), RECEIVE);

        let escrow = escrow_pda(&maker.pubkey(), SEED);
        let vault = ata(&escrow, &mint_a);

        Self {
            svm,
            maker,
            taker,
            mint_a,
            mint_b,
            maker_ata_a,
            taker_ata_b,
            escrow,
            vault,
        }
    }

    /// Runs `make` with the standard terms. Used as the starting point by every
    /// test except the one that checks `make` itself.
    fn make(&mut self) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Make {
                seed: SEED,
                receive: RECEIVE,
                deposit: DEPOSIT,
            }
            .data(),
            escrow_new::accounts::Make {
                maker: self.maker.pubkey(),
                mint_a: self.mint_a,
                mint_b: self.mint_b,
                maker_ata_a: self.maker_ata_a,
                escrow: self.escrow,
                vault: self.vault,
                associated_token_program: ATA_PROGRAM,
                token_program: TOKEN_PROGRAM,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        let maker = self.maker.insecure_clone();
        send(&mut self.svm, ix, &maker)
    }

    /// `mint_b` is a parameter so a test can hand the program a counterfeit one.
    fn take_with_mint_b(&mut self, mint_b: Pubkey) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Take {}.data(),
            escrow_new::accounts::Take {
                taker: self.taker.pubkey(),
                maker: self.maker.pubkey(),
                mint_a: self.mint_a,
                mint_b,
                taker_ata_a: ata(&self.taker.pubkey(), &self.mint_a),
                taker_ata_b: ata(&self.taker.pubkey(), &mint_b),
                maker_ata_b: ata(&self.maker.pubkey(), &mint_b),
                escrow: self.escrow,
                vault: self.vault,
                associated_token_program: ATA_PROGRAM,
                token_program: TOKEN_PROGRAM,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        let taker = self.taker.insecure_clone();
        send(&mut self.svm, ix, &taker)
    }

    fn take(&mut self) -> TransactionResult {
        self.take_with_mint_b(self.mint_b)
    }

    /// `signer` is a parameter so a test can try refunding as somebody else.
    fn refund_as(&mut self, signer: &Keypair) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Refund {}.data(),
            escrow_new::accounts::Refund {
                maker: signer.pubkey(),
                maker_ata_a: ata(&signer.pubkey(), &self.mint_a),
                mint_a: self.mint_a,
                escrow: self.escrow,
                vault: self.vault,
                associated_token_program: ATA_PROGRAM,
                token_program: TOKEN_PROGRAM,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        send(&mut self.svm, ix, signer)
    }

    fn balance(&self, token_account: &Pubkey) -> u64 {
        self.svm
            .get_account(token_account)
            .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
            .unwrap_or(0)
    }

    fn escrow_state(&self) -> Option<EscrowState> {
        let acct = self.svm.get_account(&self.escrow)?;
        EscrowState::try_deserialize(&mut acct.data.as_slice()).ok()
    }
}

// ------------------------------------------------------------------ helpers

fn send(svm: &mut LiteSVM, ix: Instruction, payer: &Keypair) -> TransactionResult {
    let msg = Message::new_with_blockhash(&[ix], Some(&payer.pubkey()), &svm.latest_blockhash());
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[payer]).unwrap();
    svm.send_transaction(tx)
}

fn escrow_pda(maker: &Pubkey, seed: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[ESCROW_SEED, maker.as_ref(), &seed.to_le_bytes()],
        &escrow_new::id(),
    )
    .0
}

/// The canonical associated token account, derived by the ATA program — note it
/// hashes under ATA_PROGRAM, not under ours.
fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

/// SPL mint layout, 82 bytes. Both authorities are left as `None`, and supply
/// stays zero — `transfer_checked` only ever reads `decimals`.
fn write_mint(svm: &mut LiteSVM, decimals: u8) -> Pubkey {
    let mut data = vec![0u8; 82];
    data[44] = decimals;
    data[45] = 1; // is_initialized

    let key = Pubkey::new_unique();
    svm.set_account(
        key,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len()),
            data,
            owner: TOKEN_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    key
}

/// SPL token account layout, 165 bytes, written at the canonical ATA address.
fn write_token_account(svm: &mut LiteSVM, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1; // AccountState::Initialized

    let key = ata(owner, mint);
    svm.set_account(
        key,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len()),
            data,
            owner: TOKEN_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    key
}

fn assert_failed_with(result: TransactionResult, needle: &str) {
    let err = format!("{:?}", result.expect_err("expected this to be rejected"));
    assert!(
        err.contains(needle),
        "expected a `{needle}` failure, got:\n{err}"
    );
}

// -------------------------------------------------------------------- tests

#[test]
fn make_locks_the_deposit_and_records_the_terms() {
    let mut w = World::new();
    w.make().unwrap();

    assert_eq!(
        w.balance(&w.vault),
        DEPOSIT,
        "deposit should sit in the vault"
    );
    assert_eq!(w.balance(&w.maker_ata_a), 0, "maker should be drained");

    // Terms the taker will be held to. `deposit` is deliberately absent from
    // state — the vault balance already records it.
    let state = w.escrow_state().expect("escrow should exist");
    assert_eq!(state.seed, SEED);
    assert_eq!(state.maker, w.maker.pubkey());
    assert_eq!(state.mint_a, w.mint_a);
    assert_eq!(state.mint_b, w.mint_b);
    assert_eq!(state.receive, RECEIVE);

    // A stored bump that disagrees with the canonical one breaks every later
    // `bump = escrow.bump` check.
    let (_, canonical) = Pubkey::find_program_address(
        &[ESCROW_SEED, w.maker.pubkey().as_ref(), &SEED.to_le_bytes()],
        &escrow_new::id(),
    );
    assert_eq!(state.bump, canonical);
}

#[test]
fn take_completes_the_swap_and_reclaims_rent() {
    let mut w = World::new();
    w.make().unwrap();

    let maker_lamports_before = w.svm.get_balance(&w.maker.pubkey()).unwrap();
    w.take().unwrap();

    // Neither of these existed before `take`; the program created them.
    assert_eq!(w.balance(&ata(&w.taker.pubkey(), &w.mint_a)), DEPOSIT);
    assert_eq!(w.balance(&ata(&w.maker.pubkey(), &w.mint_b)), RECEIVE);
    assert_eq!(w.balance(&w.taker_ata_b), 0, "taker paid in full");

    // Both accounts the maker funded are gone, and both rents came back. The
    // maker never signed this transaction, so nothing else touched the balance.
    assert!(
        w.svm.get_account(&w.escrow).is_none(),
        "escrow should be closed"
    );
    assert_eq!(w.balance(&w.vault), 0);
    assert!(
        w.svm.get_balance(&w.maker.pubkey()).unwrap() > maker_lamports_before,
        "maker should have received the reclaimed rent"
    );
}

#[test]
fn refund_returns_the_deposit_to_the_maker() {
    let mut w = World::new();
    w.make().unwrap();
    assert_eq!(w.balance(&w.maker_ata_a), 0);

    w.refund_as(&w.maker.insecure_clone()).unwrap();

    assert_eq!(
        w.balance(&w.maker_ata_a),
        DEPOSIT,
        "deposit should come back"
    );
    assert_eq!(w.balance(&w.vault), 0);
    assert!(w.svm.get_account(&w.escrow).is_none());
}

#[test]
fn take_with_a_counterfeit_mint_b_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    // A mint the taker invented. Without `has_one = mint_b` on the escrow, the
    // taker pays in this and walks off with the real deposit.
    let counterfeit = write_mint(&mut w.svm, MINT_B_DECIMALS);
    write_token_account(&mut w.svm, &counterfeit, &w.taker.pubkey(), RECEIVE);

    assert_failed_with(w.take_with_mint_b(counterfeit), "ConstraintHasOne");
    assert_eq!(w.balance(&w.vault), DEPOSIT, "vault must be untouched");
}

#[test]
fn refund_by_a_stranger_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    let stranger = Keypair::new();
    w.svm.airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();

    // The stranger signs, and passes their own ATA as the destination. The only
    // thing standing in the way is `has_one = maker`.
    assert_failed_with(w.refund_as(&stranger), "ConstraintHasOne");
    assert_eq!(w.balance(&w.vault), DEPOSIT, "vault must be untouched");
}

#[test]
fn an_escrow_cannot_be_taken_twice() {
    let mut w = World::new();
    w.make().unwrap();
    w.take().unwrap();

    // Otherwise the retry is byte-identical to the first and LiteSVM rejects it
    // as AlreadyProcessed, which would prove nothing about the program.
    w.svm.expire_blockhash();

    // `close = maker` wiped the escrow account, so the second attempt dies in
    // account validation before any transfer runs. No explicit "taken" flag.
    assert_failed_with(w.take(), "AccountNotInitialized");
    assert_eq!(w.balance(&w.taker_ata_b), 0, "taker must not pay twice");
}
