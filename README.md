# Anchor Escrow

A two-party token swap. The maker locks token A in a vault owned by a PDA and names a price in token B. Anyone who pays that price gets the deposit; until someone does, the maker can reprice the offer or walk away with the tokens.

Written with Anchor against `token_interface`, so SPL Token and Token-2022 mints both work, and every move is a `transfer_checked` carrying the mint's decimals.

```
make ── maker deposits token A, records the ask in token B
  │
  ├── take    taker pays the ask, takes the vault, both accounts close
  ├── update  maker reprices the live offer, vault untouched
  └── refund  maker takes the deposit back, both accounts close
```

## Instructions

| | Signer | Args | What it does |
|---|---|---|---|
| `make` | maker | `seed`, `receive`, `deposit`, `deadline` | Opens the escrow and its vault, moves the deposit in. Rejects a past deadline or a zero ask. |
| `take` | taker | `expected_transfer` | Pays `receive` of mint B to the maker, sweeps the vault to the taker, closes both accounts. |
| `update` | maker | `new_receive` | Rewrites the ask, amount and mint B. State only, the vault never moves. |
| `refund` | maker | | Sends the vault back to the maker, closes both accounts. |

The escrow lives at `["escrow", maker, seed]` and its vault is that PDA's ATA for mint A. The seed is caller-chosen, so one maker can run several offers on the same pair at once. There is no `deposit` field in state: the vault balance already is the deposit.

## Notes for a reviewer

**Repricing opens a front-run, so `take` is priced by the caller.** `update` can move the ask while an offer is live. A taker who reads `receive`, signs, and lands a slot later would otherwise pay whatever the maker changed it to in between. So `take` takes the price as an argument and fails with `TermsChanged` unless it still matches state. You pay the number you read or you pay nothing.

The same read covers the mint. `update` rewrites `mint_b` too, and `take` carries `has_one = mint_b`, so a taker working from stale terms is rejected in validation rather than paying in the wrong token.

**The deadline is the first dead instant, not the last live one.** Both guards read `deadline > now`, so an offer with `deadline = T` is takeable through `T - 1` and expired at `T`. Expiry closes `take` and `update`.

**Expiry takes away an option, it never strands funds.** `refund` has no deadline check at all. The maker can pull out at any point in the escrow's life, so there is no state where the vault is locked and nobody holds the key. That is also why `take` and `update` can afford to be strict about time.

**Authorization is constraints, not handler code.** `has_one = maker` gates `refund` and `update`; `has_one = mint_a` and `has_one = mint_b` pin the pair. A stranger passing their own ATAs, or a taker offering a counterfeit mint B they minted themselves, dies in account validation before a handler runs.

**Taking twice can't happen.** `take` closes the escrow, so the replay fails with `AccountNotInitialized` during validation. Nothing in the handler has to remember that it already ran.

**Rent goes back to whoever paid it.** The maker funded the escrow and vault, and both `take` and `refund` close them to the maker. The taker pays for any ATA created on the way through, including the maker's mint B account when it doesn't exist yet.

| Error | Comes from |
|---|---|
| `DeadlineInPast` | `make` with a deadline at or before the current clock |
| `EscrowExpired` | `take` or `update` at or after the deadline |
| `TermsChanged` | `take` whose `expected_transfer` no longer matches state |

## Build and test

```sh
anchor build   # writes target/deploy/escrow_new.so
cargo test     # integration tests, LiteSVM
```

Build first: the tests pull the `.so` in with `include_bytes!`.

They run on LiteSVM instead of a validator. Fixtures are packed into SVM accounts directly, and anything the program creates itself, the vault and the missing ATAs, is left out of the fixtures so `init` and `init_if_needed` stay covered. The deadline tests set `Clock.unix_timestamp` by hand, because warping slots doesn't move the wall clock the guards actually read.

The program ID in `declare_id!` is a local keypair under `target/`, which is gitignored. A fresh clone builds its own; nothing here is deployed.
