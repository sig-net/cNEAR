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
- **Events fire on every call** including no-ops. 
- **Deployment is Rust** Typed RPC instead of parsing CLI output, which resolves 4 and 6, we weren't confident in writing a secure bash deployment script.

## Recommendations not tied to a finding

Done: remediate findings; deployment script; ownership model; upgrade mechanism; pause/force-transfer events, coverage and docs; clippy, rustfmt and shellcheck gates in CI. The deployment logic is Rust now, but the justfile still decides what gets built, so its recipes are extracted and linted too.

Not done: architecture diagram, trust model, privilege matrix; monitoring, alerting and incident response (events exist, nothing consumes them); timelock; coverage gate, fuzzing, property/invariant/mutation tests.

## For a possible re-review

- **`deploy-cli/` is a rewrite of the bash script and unreviewed**.
