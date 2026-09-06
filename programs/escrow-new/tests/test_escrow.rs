//! Escrow end-to-end tests on LiteSVM.
//!
//! Fixtures are packed with spl_token's `Pack` impls and written straight into
//! the SVM. Accounts the program creates itself (vault, missing ATAs) are left
//! out so `init` / `init_if_needed` stay covered.

use {
    anchor_lang::{
        prelude::{Clock, Pubkey},
        solana_program::{
            instruction::Instruction, program_option::COption, program_pack::Pack, system_program,
        },
        AccountDeserialize, InstructionData, ToAccountMetas,
    },
    anchor_spl::token::spl_token::state::{
        Account as SplTokenAccount, AccountState, Mint as SplMint,
    },
    escrow_new::{state::EscrowState, ESCROW_SEED},
    litesvm::{types::TransactionResult, LiteSVM},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

// Not CARGO_TARGET_TMPDIR: rust-analyzer can't expand it and marks the file unreadable.
const PROGRAM_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/escrow_new.so"
));

const TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;
const ATA_PROGRAM: Pubkey = anchor_spl::associated_token::ID;

// Different on purpose: asserting the wrong mint's decimals must fail.
const MINT_A_DECIMALS: u8 = 6;
const MINT_B_DECIMALS: u8 = 9;

const SEED: u64 = 42;
const DEPOSIT: u64 = 100_000_000; // 100 of mint_a
const RECEIVE: u64 = 250_000_000_000; // 250 of mint_b
const LIFETIME: i64 = 3600;

// ---------------------------------------------------------------- fixtures

/// Maker holding mint_a, taker holding mint_b, and every address both sides need.
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
    deadline: i64,
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

        // Relative to the SVM's own clock, which does not start at today's date.
        let deadline = svm.get_sysvar::<Clock>().unix_timestamp + LIFETIME;

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
            deadline,
        }
    }

    fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    /// `warp_to_slot` moves `slot`, not `unix_timestamp` — the guards read the latter.
    fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    fn make(&mut self) -> TransactionResult {
        self.make_with_deadline(self.deadline)
    }

    fn make_with_deadline(&mut self, deadline: i64) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Make {
                seed: SEED,
                receive: RECEIVE,
                deposit: DEPOSIT,
                deadline,
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

    /// Parameterized so a test can pass a counterfeit mint or a stale price.
    fn take_with(&mut self, mint_b: Pubkey, expected_transfer: u64) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Take { expected_transfer }.data(),
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
        self.take_with(self.mint_b, RECEIVE)
    }

    fn update(&mut self, signer: &Keypair, new_receive: u64) -> TransactionResult {
        let ix = Instruction::new_with_bytes(
            escrow_new::id(),
            &escrow_new::instruction::Update { new_receive }.data(),
            escrow_new::accounts::Update {
                maker: signer.pubkey(),
                mint_b: self.mint_b,
                escrow: self.escrow,
                token_program: TOKEN_PROGRAM,
            }
            .to_account_metas(None),
        );
        send(&mut self.svm, ix, signer)
    }

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

    /// Zero for a missing account; existence is asserted separately where it matters.
    fn balance(&self, token_account: &Pubkey) -> u64 {
        token_amount(&self.svm, token_account)
    }

    fn supply(&self, mint: &Pubkey) -> u64 {
        SplMint::unpack(&self.svm.get_account(mint).unwrap().data)
            .unwrap()
            .supply
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

/// Canonical ATA — hashes under ATA_PROGRAM, not ours.
fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

fn write_spl<T: Pack>(svm: &mut LiteSVM, key: Pubkey, state: T) -> Pubkey {
    let mut data = vec![0u8; T::LEN];
    state.pack_into_slice(&mut data);

    svm.set_account(
        key,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(T::LEN),
            data,
            owner: TOKEN_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    key
}

/// Nothing here mints, so there is no authority to model. Supply starts at zero
/// and `write_token_account` keeps it in step with the balances it writes.
fn write_mint(svm: &mut LiteSVM, decimals: u8) -> Pubkey {
    write_spl(
        svm,
        Pubkey::new_unique(),
        SplMint {
            decimals,
            is_initialized: true,
            supply: 0,
            mint_authority: COption::None,
            freeze_authority: COption::None,
        },
    )
}

fn token_amount(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key)
        .and_then(|a| SplTokenAccount::unpack(&a.data).ok())
        .map_or(0, |a| a.amount)
}

/// Writing balances directly bypasses `MintTo`, so the mint's supply is adjusted
/// by hand to stay equal to the sum of every balance written for it. Nothing in
/// the escrow reads supply, but a fixture that contradicts itself is a trap.
fn write_token_account(svm: &mut LiteSVM, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
    let key = ata(owner, mint);
    let previous = token_amount(svm, &key);

    let mut state =
        SplMint::unpack(&svm.get_account(mint).expect("mint must exist first").data).unwrap();
    state.supply = state.supply + amount - previous;
    write_spl(svm, *mint, state);

    write_spl(
        svm,
        key,
        SplTokenAccount {
            mint: *mint,
            owner: *owner,
            amount,
            state: AccountState::Initialized,
            delegate: COption::None,
            delegated_amount: 0,
            is_native: COption::None,
            close_authority: COption::None,
        },
    )
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

    // Transfers are supply-neutral: only MintTo and Burn move this number.
    assert_eq!(w.supply(&w.mint_a), DEPOSIT);
    assert_eq!(w.supply(&w.mint_b), RECEIVE);

    // No `deposit` field: the vault balance already records it.
    let state = w.escrow_state().expect("escrow should exist");
    assert_eq!(state.seed, SEED);
    assert_eq!(state.maker, w.maker.pubkey());
    assert_eq!(state.mint_a, w.mint_a);
    assert_eq!(state.mint_b, w.mint_b);
    assert_eq!(state.receive, RECEIVE);

    // A non-canonical stored bump breaks every later `bump = escrow.bump`.
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

    // Created by `init_if_needed` during take.
    assert_eq!(w.balance(&ata(&w.taker.pubkey(), &w.mint_a)), DEPOSIT);
    assert_eq!(w.balance(&ata(&w.maker.pubkey(), &w.mint_b)), RECEIVE);
    assert_eq!(w.balance(&w.taker_ata_b), 0, "taker paid in full");

    // Maker never signed, so the balance rise is purely reclaimed rent.
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

    // Without `has_one = mint_b` this buys the real deposit for nothing.
    let counterfeit = write_mint(&mut w.svm, MINT_B_DECIMALS);
    write_token_account(&mut w.svm, &counterfeit, &w.taker.pubkey(), RECEIVE);

    assert_failed_with(w.take_with(counterfeit, RECEIVE), "ConstraintHasOne");
    assert_eq!(w.balance(&w.vault), DEPOSIT, "vault must be untouched");
}

#[test]
fn refund_by_a_stranger_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    let stranger = Keypair::new();
    w.svm.airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();

    // Stranger signs and passes their own ATA; `has_one = maker` is the only guard.
    assert_failed_with(w.refund_as(&stranger), "ConstraintHasOne");
    assert_eq!(w.balance(&w.vault), DEPOSIT, "vault must be untouched");
}

#[test]
fn an_escrow_cannot_be_taken_twice() {
    let mut w = World::new();
    w.make().unwrap();
    w.take().unwrap();

    // Else the retry is byte-identical and dies as AlreadyProcessed, proving nothing.
    w.svm.expire_blockhash();

    // `close = maker` wiped the account, so validation rejects it before any transfer.
    assert_failed_with(w.take(), "AccountNotInitialized");
    assert_eq!(w.balance(&w.taker_ata_b), 0, "taker must not pay twice");
}

#[test]
fn maker_can_reprice_and_the_next_taker_pays_the_new_price() {
    let mut w = World::new();
    w.make().unwrap();

    let new_price = RECEIVE * 2;
    write_token_account(&mut w.svm, &w.mint_b, &w.taker.pubkey(), new_price);
    // Overwrites an existing balance, so supply must move by the delta, not the total.
    assert_eq!(w.supply(&w.mint_b), new_price);

    let maker = w.maker.insecure_clone();
    w.update(&maker, new_price).unwrap();
    assert_eq!(w.escrow_state().unwrap().receive, new_price);

    // update rewrites state only.
    assert_eq!(w.balance(&w.vault), DEPOSIT);

    w.take_with(w.mint_b, new_price).unwrap();
    assert_eq!(w.balance(&ata(&w.maker.pubkey(), &w.mint_b)), new_price);
}

#[test]
fn take_is_rejected_when_the_maker_repriced_mid_flight() {
    let mut w = World::new();
    w.make().unwrap();

    // Maker raises the ask after the taker committed to RECEIVE.
    let maker = w.maker.insecure_clone();
    w.update(&maker, RECEIVE * 10).unwrap();

    assert_failed_with(w.take_with(w.mint_b, RECEIVE), "TermsChanged");
    assert_eq!(
        w.balance(&w.taker_ata_b),
        RECEIVE,
        "taker must not have paid"
    );
    assert_eq!(w.balance(&w.vault), DEPOSIT, "escrow still open");
}

#[test]
fn update_by_a_stranger_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    let stranger = Keypair::new();
    w.svm.airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();

    assert_failed_with(w.update(&stranger, 1), "ConstraintHasOne");
    assert_eq!(
        w.escrow_state().unwrap().receive,
        RECEIVE,
        "terms must be unchanged"
    );
}

#[test]
fn make_with_a_past_deadline_is_rejected() {
    let mut w = World::new();
    let past = w.now() - 1;

    assert_failed_with(w.make_with_deadline(past), "DeadlineInPast");
    assert!(w.svm.get_account(&w.escrow).is_none());
    assert_eq!(w.balance(&w.maker_ata_a), DEPOSIT, "deposit never moved");
}

#[test]
fn take_at_the_deadline_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    // The deadline is the first invalid instant, not the last valid one.
    w.set_time(w.deadline);

    assert_failed_with(w.take(), "EscrowExpired");
    assert_eq!(w.balance(&w.vault), DEPOSIT, "vault must be untouched");
    assert_eq!(
        w.balance(&w.taker_ata_b),
        RECEIVE,
        "taker must not have paid"
    );
}

#[test]
fn take_one_second_before_the_deadline_still_works() {
    let mut w = World::new();
    w.make().unwrap();

    w.set_time(w.deadline - 1);

    w.take().unwrap();
    assert_eq!(w.balance(&ata(&w.taker.pubkey(), &w.mint_a)), DEPOSIT);
}

#[test]
fn refund_after_the_deadline_still_works() {
    let mut w = World::new();
    w.make().unwrap();

    // The point of the lazy design: expiry removes an option, it never strands funds.
    w.set_time(w.deadline + LIFETIME);

    w.refund_as(&w.maker.insecure_clone()).unwrap();
    assert_eq!(w.balance(&w.maker_ata_a), DEPOSIT);
    assert!(w.svm.get_account(&w.escrow).is_none());
}

#[test]
fn update_after_the_deadline_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();
    w.set_time(w.deadline);

    let maker = w.maker.insecure_clone();
    assert_failed_with(w.update(&maker, RECEIVE * 2), "EscrowExpired");
    assert_eq!(w.escrow_state().unwrap().receive, RECEIVE);
}

#[test]
fn update_to_a_zero_price_is_rejected() {
    let mut w = World::new();
    w.make().unwrap();

    // A zero ask drains the vault for free.
    let maker = w.maker.insecure_clone();
    assert_failed_with(w.update(&maker, 0), "RequireGtViolated");
    assert_eq!(w.escrow_state().unwrap().receive, RECEIVE);
}
