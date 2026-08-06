# cNEAR remediations

Response to the Trail of Bits assessment (2026-07-27) of cNEAR at `c1944263327d8b4cb0b640824b33f37f78d7108d`. All 14 findings addressed, one commit each.

## Findings

| # | Sev | Finding | Proven by |
|---|---|---|---|
| 1 | Info | Vulnerable locked dependencies | CI (`cargo audit`) |
| 2 | Info | Deployment retains access keys | `removes_every_key_from_the_token_account`, `is_a_no_op_on_an_account_that_is_already_keyless` |
| 3 | Info | Default supply is one-billionth of a cNEAR | `deployment_registers_and_transfers_ownership`, `whole_tokens_scale_by_the_decimals` |
| 4 | Low | RPC errors read as "account exists" | `only_the_not_found_condition_counts_as_absence`, `finds_the_not_found_condition_through_a_source_chain` |
| 5 | Low | Deployment omits controller registration | `deployment_registers_and_transfers_ownership`, `test_controller_deploys_working_token` |
| 6 | Low | Ownership verification doesn't fail deployment | `ownership_verification_fails_when_the_owner_is_wrong` |
| 7 | **High** | Controller upgrades deploy malformed code | `test_controller_upgrade_leaves_working_token`, `test_controller_delegates_token_control` |
| 8 | Info | Unpinned controller source | `controller_wasm_is_built_from_the_pinned_commit` |
| 9 | Info | `owner_get` disagrees with effective ownership | `test_ownership_transfer_leaves_previous_owner_powerless`, `test_trait_owner_set_is_unsupported` |
| 10 | Info | Freeze-list uses contract-funded storage | documentation only |
| 11 | Low | Admin actions emit no events | 7 event tests |
| 12 | Info | Force transfers bypass pause/freeze | `test_freeze_prevents_transfers` |
| 13 | **High** | Transfer retains previous owner's permissions | `test_previous_owner_cannot_act_after_transfer`, `test_previous_owner_cannot_reclaim_ownership` |
| 14 | Med | Frozen accounts burn balance via `storage_unregister` | 4 unit, 2 sandbox tests |

## Behaviour changes

- **Single owner, two-step transfer** as recommended in the audit.
- **`pause` removed** in favour of `pause_contract`, the only name `delegate_pause` accepts. One entry point as the report asked.
- **Events fire on every call**  including no-ops. 
- **Deployment is Rust** Typed RPC instead of parsing CLI output, which resolves 4 and 6, we weren't confident in writing a secure bash deployment script.

## Recommendations not tied to a finding

Done: remediate findings; deployment script; ownership model; upgrade mechanism; pause/force-transfer events, coverage and docs; clippy, rustfmt and shellcheck gates in CI.
The deployment logic is Rust now, but the justfile still decides what gets
built, so its recipes are extracted and linted too.

Not done: architecture diagram, trust model, privilege matrix; monitoring, alerting and incident response (events exist, nothing consumes them); timelock; coverage gate, fuzzing, property/invariant/mutation tests.

## Notes on the four that were hardest to prove

- **2** — `finalize` deletes access keys, so the test asserts the account really is keyless afterwards, and that a second run is a no-op for an operator unsure whether the first completed.
- **4** — the not-found condition is tested against the failures that must *not* count as absence (unreachable endpoint, timeout, 429) and against a nested source chain, which is where the condition actually appears.
- **6** — verification was extracted so the failing case can be driven directly, and is asserted in both directions.
- **8** — the checkout is asserted to sit at the pinned commit and the wasm not to be an empty leftover. This is the incident we had: a silently failed build left an artifact from a previously pinned commit serving the test suite for a day.

## For a possible re-review

- **`deploy-cli/` is a rewrite of the bash script and unreviewed**.
- **The controller pin is not the reviewed revision**: `351dc02`, on Aurora's recommendation, and it moves again once our upstream dependency bump merges.
- **The 100 trillion initial supply is a product decision.** The report reasoned about one billion. Someone who owns cNEAR's economics should confirm it rather than inherit it from a remediation commit.

## Other problems found while remediating

None of these are in the report. They were found by fixing what was, and are
listed because several are the same class of failure the assessment is about:
tooling that reports success without having checked anything.

**The controller build had been failing silently, and the test suite was
running against a stale binary.** `build-controller` ended in
`cp … 2>/dev/null || true` with no `set -e`, so the recipe exited 0 whether or
not the build worked. When the pin changed, the new revision could not be built
at all — and nothing noticed, because the wasm from the previously pinned commit
was still sitting in `target/near`. Every controller test for a day ran against
the wrong artifact. Fixed, and now asserted by a test (TOB-CNEAR-8).

**The pinned controller cannot be built with a current toolchain.** Its
near-sdk declares no `min_protocol_version`, so cargo-near applies a 1.86 rustc
ceiling — while this repository is on 1.93. The ceiling is stale metadata
rather than a live constraint: mainnet is on protocol 86 and the VM accepts the
newer opcodes. Worked around by building the controller in its own devshell;
fixed properly by
[an upstream dependency bump](https://github.com/sig-net/aurora-controller-factory/tree/deps/modern-near-sdk)
awaiting review. Remove the second toolchain once that merges.

**The lockfile had crossed the MSRV boundary.** `Cargo.lock` contained crates
requiring rustc 1.93 while `rust-toolchain.toml` pinned 1.86, so
`cargo check --locked` failed outright — someone had run `cargo update` under a
newer compiler than the repository pins. Resolved by the move to 1.93.

**`upgrade` was exported as a view method.** Taking `&self` made near-sdk
classify the contract's most dangerous method as view-kind, so the ABI
advertised it as a free query, and near-sdk's non-payable deposit check never
ran. Now `&mut self` and `#[payable]`.

**The mainnet dry run understated the cost of deploying by about 3.2 NEAR.**
The preview printed `add_release_blob` with no `--amount`, though the real run
attaches a deposit sized to the wasm. An operator budgeting from the preview
would have come up short. The printed deposit is now derived from the artifact
on disk. The same command was also unpasteable: the deposit was rendered as
`3.213 NEAR`, and `--amount` takes a bare number.

**Assertion messages were printing their own placeholders.** On this edition,
`assert!(cond, "value = {value}")` does not interpolate, so several tests
printed the literal text instead of the number — exactly when the number was
what you needed. Fixed where found; worth watching for in new tests.

**Sandbox tests contend rather than fail.** Each starts its own `neard`;
running them all at once races on unpacking the shared `near-sandbox` binary
and on binding ports. It presents as flaky tests. CI now runs two at a time.

**Storage test tolerances were calibrated against an older sandbox.** nearcore
2.13 defers gas-refund receipts and pays larger gas rewards, so assertions that
measured balances immediately after a call failed. The tests now let refunds
settle and carry bounds derived from measured values rather than inherited
ones.

**Still open: the release version recorded at deployment is hardcoded.**
`deploy-cli` registers the deployment as version `1.0.0` regardless of the
token crate's actual version. If the crate version moves and this does not, the
controller's registry will misreport what is deployed.
